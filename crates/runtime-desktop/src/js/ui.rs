//! Desktop `phoenix.ui` bindings backed by [`phoenix_ui_desktop`] and native
//! GPUI message accessories ([`crate::accessories`]).
//!
//! Native clipboard, toasts, notifications, and `addMessageAccessory` run live
//! on Desktop Phoenix. Remaining mobile-only UI-op slot hooks return harmless
//! no-op removers so cross-platform plugins load cleanly without throwing.

use std::sync::atomic::{
   AtomicU64,
   Ordering,
};

use phoenix_runtime::{
   discord::RequestAction,
   js::{
      Engine,
      Registry,
   },
};
use rquickjs::{
   CatchResultExt,
   Ctx,
   Exception,
   Function,
   Object,
   Value,
   function::Opt,
};

use crate::{
   accessories::{
      self,
      RenderedAccessory,
   },
   js::Desktop,
};

#[derive(Clone)]
pub struct DesktopAccessory<'js> {
   pub id:       u64,
   pub plugin:   String,
   pub priority: i32,
   pub render:   Function<'js>,
}

fn noop_handle<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Function<'js>> {
   let remove = Function::new(ctx.clone(), || {})?;
   remove.set("update", Function::new(ctx.clone(), || {})?)?;
   remove.set("close", Function::new(ctx.clone(), || {})?)?;
   Ok(remove)
}

fn parse_rendered(value: &Value<'_>, priority: i32) -> Option<RenderedAccessory> {
   if value.is_null() || value.is_undefined() || value.as_bool() == Some(false) {
      return None;
   }
   if let Some(s) = value.as_string() {
      let text = s.to_string().ok()?;
      if text.is_empty() {
         return None;
      }
      return Some(RenderedAccessory {
         priority,
         text,
         color: None,
         tint: None,
      });
   }
   if let Some(obj) = value.as_object() {
      let text = obj
         .get::<_, Option<String>>("text")
         .ok()
         .flatten()
         .unwrap_or_default();
      let color = obj
         .get::<_, Option<f64>>("color")
         .ok()
         .flatten()
         .map(|n| n as i64 as u32);
      let tint = obj
         .get::<_, Option<f64>>("tint")
         .ok()
         .flatten()
         .map(|n| n as i64 as u32);
      if text.is_empty() && tint.is_none() {
         return None;
      }
      return Some(RenderedAccessory {
         priority,
         text,
         color,
         tint,
      });
   }
   None
}

fn evaluate_accessories_in_ctx<'js>(ctx: &Ctx<'js>, message_id: u64) -> Vec<RenderedAccessory> {
   let Some(registry) = ctx.userdata::<Desktop<'js>>() else {
      return Vec::new();
   };
   let snapshot: Vec<DesktopAccessory<'js>> = registry.accessories.borrow().clone();
   drop(registry);
   if snapshot.is_empty() {
      return Vec::new();
   }

   let id_str = message_id.to_string();
   let Ok(arg) = Object::new(ctx.clone()) else {
      return Vec::new();
   };
   let _ = arg.set("messageId", id_str.as_str());
   let _ = arg.set("channelId", Value::new_null(ctx.clone()));
   let _ = arg.set("currentUserId", Value::new_null(ctx.clone()));
   if let Ok(reply) = Function::new(ctx.clone(), || String::new()) {
      let _ = arg.set("reply", reply);
   }

   let mut out = Vec::new();
   for acc in snapshot {
      match acc.render.call::<_, Value<'_>>((arg.clone(),)).catch(ctx) {
         Ok(val) if val.is_promise() => {
            if let Some(promise) = val.as_object()
               && let Ok(then) = promise.get::<_, Function<'_>>("then")
            {
               let mid = message_id;
               let prio = acc.priority;
               if let Ok(on_resolve) =
                  Function::new(ctx.clone(), move |ctx: Ctx<'js>, resolved: Value<'js>| {
                     if let Some(item) = parse_rendered(&resolved, prio) {
                        let mut all = evaluate_sync_only(&ctx, mid, Some((prio, item)));
                        all.sort_by_key(|a| a.priority);
                        accessories::set_rendered(mid, all);
                     }
                  })
               {
                  let _ = then.call::<_, Value<'_>>((on_resolve,));
               }
            }
         },
         Ok(val) => {
            if let Some(rendered) = parse_rendered(&val, acc.priority) {
               out.push(rendered);
            }
         },
         Err(err) => {
            log::error!("[{}] addMessageAccessory render: {err}", acc.plugin);
         },
      }
   }
   out
}

fn evaluate_sync_only<'js>(
   ctx: &Ctx<'js>,
   message_id: u64,
   extra: Option<(i32, RenderedAccessory)>,
) -> Vec<RenderedAccessory> {
   let mut out = Vec::new();
   if let Some((_, item)) = extra {
      out.push(item);
   }
   let Some(registry) = ctx.userdata::<Desktop<'js>>() else {
      return out;
   };
   let snapshot: Vec<DesktopAccessory<'js>> = registry.accessories.borrow().clone();
   drop(registry);

   let id_str = message_id.to_string();
   let Ok(arg) = Object::new(ctx.clone()) else {
      return out;
   };
   let _ = arg.set("messageId", id_str.as_str());
   let _ = arg.set("channelId", Value::new_null(ctx.clone()));
   let _ = arg.set("currentUserId", Value::new_null(ctx.clone()));

   for acc in snapshot {
      if let Ok(val) = acc.render.call::<_, Value<'_>>((arg.clone(),)).catch(ctx)
         && !val.is_promise()
         && let Some(rendered) = parse_rendered(&val, acc.priority)
         && !out.contains(&rendered)
      {
         out.push(rendered);
      }
   }
   out
}

/// Evaluates every registered desktop `addMessageAccessory` for `message_id`.
pub fn render_accessories(engine: &Engine, message_id: u64) -> Vec<RenderedAccessory> {
   engine.with(|ctx| evaluate_accessories_in_ctx(&ctx, message_id))
}

fn add_message_accessory<'js>(
   ctx: &Ctx<'js>,
   plugin: &str,
   options: Object<'js>,
) -> rquickjs::Result<Function<'js>> {
   static NEXT_ID: AtomicU64 = AtomicU64::new(1);

   let render = options.get::<_, Function<'js>>("render")?;
   let priority = options.get::<_, Option<i32>>("priority")?.unwrap_or(0);
   let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);

   let count = {
      let registry = ctx
         .userdata::<Desktop<'js>>()
         .ok_or_else(|| Exception::throw_internal(ctx, "no registry"))?;
      let mut list = registry.accessories.borrow_mut();
      list.push(DesktopAccessory {
         id,
         plugin: plugin.to_owned(),
         priority,
         render,
      });
      list.len()
   };
   accessories::set_active_count(count);
   accessories::clear_cache();

   let remove = Function::new(ctx.clone(), move |ctx: Ctx<'js>| {
      if let Some(registry) = ctx.userdata::<Desktop<'js>>() {
         let mut list = registry.accessories.borrow_mut();
         list.retain(|entry| entry.id != id);
         accessories::set_active_count(list.len());
         accessories::clear_cache();
         accessories::invalidate_discord_windows();
      }
   })?;
   remove.set(
      "update",
      Function::new(ctx.clone(), |ctx: Ctx<'js>, message_id: Opt<String>| {
         if let Some(mid) = message_id.0.and_then(|s| s.parse::<u64>().ok()) {
            let items = evaluate_accessories_in_ctx(&ctx, mid);
            accessories::set_rendered(mid, items);
         } else {
            accessories::clear_cache();
            accessories::invalidate_discord_windows();
         }
      })?,
   )?;
   Ok(remove)
}

/// Wires desktop `phoenix.ui`, `phoenix.messages.onSend` / `onEmojiPick`, and
/// `phoenix.commands` onto the plugin's API objects.
pub fn desktop_patch_api<'js>(
   ctx: &Ctx<'js>,
   plugin: &str,
   api: &Object<'js>,
   messages: &Object<'js>,
) -> rquickjs::Result<()> {
   let name = plugin.to_owned();
   messages.set(
      "onSend",
      Function::new(
         ctx.clone(),
         move |ctx: Ctx<'js>, function: Function<'js>| on_send(&ctx, &name, function),
      )?,
   )?;
   messages.set(
      "onEmojiPick",
      Function::new(ctx.clone(), |ctx: Ctx<'js>, _function: Opt<Value<'js>>| {
         noop_handle(&ctx)
      })?,
   )?;
   let commands = Object::new(ctx.clone())?;
   commands.set(
      "register",
      Function::new(ctx.clone(), |ctx: Ctx<'js>, _options: Opt<Value<'js>>| {
         noop_handle(&ctx)
      })?,
   )?;
   commands.set(
      "list",
      Function::new(ctx.clone(), |ctx: Ctx<'js>| rquickjs::Array::new(ctx))?,
   )?;
   api.set("commands", commands)?;
   api.set("ui", desktop_ui_object(ctx, plugin)?)
}

pub fn desktop_ui_object<'js>(ctx: &Ctx<'js>, plugin: &str) -> rquickjs::Result<Object<'js>> {
   let ui = Object::new(ctx.clone())?;
   ui.set(
      "toast",
      Function::new(ctx.clone(), |message: String| {
         phoenix_ui_desktop::toast(&message);
      })?,
   )?;
   ui.set(
      "copy",
      Function::new(ctx.clone(), |ctx: Ctx<'js>, text: String| {
         phoenix_ui_desktop::copy(&text)
            .map_err(|err| Exception::throw_message(&ctx, &err.to_string()))
      })?,
   )?;
   ui.set(
      "notify",
      Function::new(ctx.clone(), |ctx: Ctx<'js>, options: Object<'js>| {
         let title = options
            .get::<_, Option<String>>("title")?
            .unwrap_or_default();
         let body = options.get::<_, Option<String>>("body")?;
         phoenix_ui_desktop::notify(&title, body.as_deref());
         noop_handle(&ctx)
      })?,
   )?;

   let owner = plugin.to_owned();
   ui.set(
      "addMessageAccessory",
      Function::new(ctx.clone(), move |ctx: Ctx<'js>, options: Object<'js>| {
         add_message_accessory(&ctx, &owner, options)
      })?,
   )?;

   // Graceful no-op shims for remaining mobile UI-op surfaces so cross-platform
   // plugins load without throwing on desktop.
   for slot_fn in [
      "addHeaderButton",
      "addComposerButton",
      "addMessageAction",
      "addProfileSection",
      "addProfileTab",
      "hide",
      "patchProps",
      "onEvent",
      "onNode",
      "mount",
      "showSheet",
      "dispatch",
      "update",
   ] {
      ui.set(
         slot_fn,
         Function::new(ctx.clone(), |ctx: Ctx<'js>| noop_handle(&ctx))?,
      )?;
   }
   for query_fn in ["find", "findDown"] {
      ui.set(
         query_fn,
         Function::new(ctx.clone(), |ctx: Ctx<'js>| rquickjs::Array::new(ctx))?,
      )?;
   }
   for nullable_fn in ["node", "root"] {
      ui.set(
         nullable_fn,
         Function::new(ctx.clone(), |ctx: Ctx<'js>| Value::new_null(ctx))?,
      )?;
   }
   ui.set(
      "openPage",
      Function::new(ctx.clone(), |ctx: Ctx<'js>, _page: Opt<Value<'js>>| {
         log::info!("desktop ui: openPage is not yet implemented on desktop");
         noop_handle(&ctx)
      })?,
   )?;
   ui.set(
      "choose",
      Function::new(
         ctx.clone(),
         |ctx: Ctx<'js>, _options: Opt<Value<'js>>| -> rquickjs::Result<rquickjs::Promise<'js>> {
            let (promise, _resolve, reject) = ctx.promise()?;
            let err =
               Exception::from_message(ctx, "phoenix.ui.choose is not yet implemented on desktop")?;
            reject.call::<_, ()>((err,))?;
            Ok(promise)
         },
      )?,
   )?;
   Ok(ui)
}

/// Runs registered `phoenix.messages.onSend` hooks against the JSON payload
/// of an outgoing `POST /channels/<channel_id>/messages` request on desktop.
pub fn intercept_send_body(engine: &Engine, channel_id: &str, body: &[u8]) -> RequestAction {
   engine.with(|ctx| {
      let Some(registry) = ctx.userdata::<Registry<'_>>() else {
         return RequestAction::Pass;
      };
      let interceptors = registry
         .interceptors
         .borrow()
         .iter()
         .map(|(_, plugin, function)| (plugin.clone(), function.clone()))
         .collect::<Vec<_>>();
      drop(registry);
      if interceptors.is_empty() {
         return RequestAction::Pass;
      }
      let Ok(mut payload) = serde_json::from_slice::<serde_json::Value>(body) else {
         return RequestAction::Pass;
      };
      let Some(original) = payload
         .get("content")
         .and_then(serde_json::Value::as_str)
         .map(str::to_owned)
      else {
         return RequestAction::Pass;
      };
      let Ok(argument) = Object::new(ctx.clone()) else {
         return RequestAction::Pass;
      };
      let _ = argument.set("channelId", channel_id);

      let mut value = original.clone();
      for (plugin, function) in interceptors {
         match function
            .call::<_, Value<'_>>((value.clone(), argument.clone()))
            .catch(&ctx)
         {
            Ok(result) if result.as_bool() == Some(false) => {
               return RequestAction::Drop;
            },
            Ok(result) => {
               if let Some(text) = result.as_string().and_then(|text| text.to_string().ok()) {
                  value = text;
               }
            },
            Err(err) => log::error!("[{plugin}] onSend: {err}"),
         }
      }
      if value == original {
         return RequestAction::Pass;
      }
      if let Some(obj) = payload.as_object_mut() {
         obj.insert("content".to_owned(), serde_json::Value::String(value));
      }
      serde_json::to_vec(&payload).map_or(RequestAction::Pass, RequestAction::Replace)
   })
}

pub fn on_send<'js>(
   ctx: &Ctx<'js>,
   plugin: &str,
   function: Function<'js>,
) -> rquickjs::Result<Function<'js>> {
   static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

   let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
   let registry = ctx
      .userdata::<Registry<'js>>()
      .ok_or_else(|| Exception::throw_internal(ctx, "no registry"))?;
   registry
      .interceptors
      .borrow_mut()
      .push((id, plugin.to_owned(), function));
   drop(registry);
   Function::new(ctx.clone(), move |ctx: Ctx<'js>| {
      if let Some(registry) = ctx.userdata::<Registry<'js>>() {
         registry
            .interceptors
            .borrow_mut()
            .retain(|(existing, ..)| *existing != id);
      }
   })
}
