//! `phoenix.messages`, `phoenix.commands`, send interceptors and emoji
//! picks.

#[cfg(target_os = "android")] use rquickjs::CatchResultExt;
use rquickjs::{
   Ctx,
   Exception,
   Function,
   Object,
   Value,
};
use serde_json::value::RawValue;

use crate::local;
#[cfg(target_os = "android")]
use crate::{
   js::{
      Engine,
      Registry,
      drain,
      ui::{
         context_object,
         remover,
         ui_object,
      },
   },
   patches::{
      self,
      Command,
      Sent,
      UiContext,
   },
};

#[cfg(target_os = "android")]
impl Engine {
   /// Runs the slash command `text` names, if any, then every send
   /// interceptor.
   pub fn submit(&self, text: &str, context: &UiContext) -> Sent {
      let sent = self.context.with(|ctx| {
         let Some(registry) = ctx.userdata::<Registry<'_>>() else {
            return Sent::Unchanged;
         };
         let argument = match context_object(&ctx, context) {
            Ok(argument) => argument,
            Err(err) => {
               log::error!("send context: {err}");
               return Sent::Unchanged;
            },
         };
         let mut value = text.to_owned();
         let invoked = text.strip_prefix('/').and_then(|rest| {
            let (name, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            let commands = registry.commands.borrow();
            let (plugin, execute) = commands.get(&name.to_lowercase())?;
            Some((
               name.to_owned(),
               args.trim().to_owned(),
               plugin.clone(),
               execute.clone(),
            ))
         });
         if let Some((name, args, plugin, execute)) = invoked {
            let result = match execute
               .call::<_, Value<'_>>((args, argument.clone()))
               .catch(&ctx)
            {
               Ok(result) => result,
               Err(err) => {
                  log::error!("[{plugin}] /{name}: {err}");
                  return Sent::Cancel;
               },
            };
            if result.is_promise() {
               return Sent::Cancel;
            }
            let outgoing = match result.as_object() {
               Some(object) if !result.is_string() => {
                  object.get::<_, Option<String>>("content").ok().flatten()
               },
               _ => result.as_string().and_then(|text| text.to_string().ok()),
            };
            match outgoing {
               Some(outgoing) if !outgoing.is_empty() => value = outgoing,
               _ => return Sent::Cancel,
            }
         }
         let interceptors = registry
            .interceptors
            .borrow()
            .iter()
            .map(|(_, plugin, function)| (plugin.clone(), function.clone()))
            .collect::<Vec<_>>();
         drop(registry);
         for (plugin, function) in interceptors {
            match function
               .call::<_, Value<'_>>((value.clone(), argument.clone()))
               .catch(&ctx)
            {
               Ok(result) if result.as_bool() == Some(false) => return Sent::Cancel,
               Ok(result) => {
                  if let Some(text) = result.as_string().and_then(|text| text.to_string().ok()) {
                     value = text;
                  }
               },
               Err(err) => log::error!("[{plugin}] onSend: {err}"),
            }
         }
         if value == text {
            Sent::Unchanged
         } else {
            Sent::Text(value)
         }
      });
      drain(&self.runtime);
      sent
   }

   /// Asks the `onEmojiPick` handlers about a press on custom emoji `id` in
   /// the picker. Returns the text the first one puts in the message box.
   #[expect(
      clippy::needless_collect,
      reason = "the registry borrow must drop before the handlers run"
   )]
   pub fn pick_emoji(&self, id: &str, context: &UiContext) -> Option<String> {
      let picked = self.context.with(|ctx| {
         let registry = ctx.userdata::<Registry<'_>>()?;
         let argument = context_object(&ctx, context).ok()?;
         let pickers = registry
            .pickers
            .borrow()
            .iter()
            .map(|(_, plugin, function)| (plugin.clone(), function.clone()))
            .collect::<Vec<_>>();
         drop(registry);
         pickers.into_iter().find_map(|(plugin, function)| {
            match function
               .call::<_, Value<'_>>((id, argument.clone()))
               .catch(&ctx)
            {
               Ok(result) => result.as_string().and_then(|text| text.to_string().ok()),
               Err(err) => {
                  log::error!("[{plugin}] onEmojiPick: {err}");
                  None
               },
            }
         })
      });
      drain(&self.runtime);
      picked
   }
}

/// `phoenix.messages`, local-only messages.
pub fn messages_object<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Object<'js>> {
   let messages = Object::new(ctx.clone())?;
   messages.set(
      "addLocal",
      Function::new(
         ctx.clone(),
         |ctx: Ctx<'js>,
          channel_id: String,
          content: String,
          options: rquickjs::function::Opt<Object<'js>>| {
            let (author, guild_id, embeds) = match options.0 {
               Some(options) => {
                  (
                     options.get::<_, Option<String>>("author")?,
                     options.get::<_, Option<String>>("guildId")?,
                     options.get::<_, Option<Value<'js>>>("embeds")?,
                  )
               },
               None => (None, None, None),
            };
            let embeds = match embeds {
               Some(embeds) => {
                  ctx.json_stringify(embeds)?
                     .map(|json| json.to_string())
                     .transpose()?
               },
               None => None,
            };
            let embeds = RawValue::from_string(embeds.unwrap_or_else(|| "[]".to_owned()))
               .map_err(|err| Exception::throw_type(&ctx, &format!("embeds: {err}")))?;
            let options = local::Options {
               author:   author.as_deref().unwrap_or("Phoenix"),
               guild_id: guild_id.as_deref(),
               embeds:   &embeds,
            };
            local::add(&channel_id, &content, &options)
               .ok_or_else(|| Exception::throw_message(&ctx, "this build has no MESSAGE_CREATE"))
         },
      )?,
   )?;
   messages.set(
      "updateLocal",
      Function::new(ctx.clone(), |ctx: Ctx<'js>, message: Value<'js>| {
         let json = ctx
            .json_stringify(message)?
            .ok_or_else(|| Exception::throw_type(&ctx, "updateLocal needs a message object"))?
            .to_string()?;
         Ok::<_, rquickjs::Error>(local::update(json))
      })?,
   )?;
   messages.set(
      "deleteLocal",
      Function::new(ctx.clone(), |channel_id: String, id: String| {
         local::delete(&channel_id, &id)
      })?,
   )?;
   Ok(messages)
}

/// `phoenix.ui`, `phoenix.commands` and `phoenix.messages.onSend`.
#[cfg(target_os = "android")]
pub fn patch_api<'js>(
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
   let name = plugin.to_owned();
   messages.set(
      "onEmojiPick",
      Function::new(
         ctx.clone(),
         move |ctx: Ctx<'js>, function: Function<'js>| on_emoji_pick(&ctx, &name, function),
      )?,
   )?;
   api.set("ui", ui_object(ctx, plugin)?)?;
   api.set("commands", commands_object(ctx, plugin)?)
}

/// `phoenix.commands`, slash commands typed in the message box.
#[cfg(target_os = "android")]
fn commands_object<'js>(ctx: &Ctx<'js>, plugin: &str) -> rquickjs::Result<Object<'js>> {
   let commands = Object::new(ctx.clone())?;
   let owner = plugin.to_owned();
   commands.set(
      "register",
      Function::new(ctx.clone(), move |ctx: Ctx<'js>, options: Object<'js>| {
         let name = options
            .get::<_, Option<String>>("name")?
            .map(|name| name.trim_start_matches('/').to_lowercase())
            .filter(|name| !name.is_empty() && !name.contains(char::is_whitespace));
         let execute = options.get::<_, Option<Function<'js>>>("execute")?;
         let (Some(name), Some(execute)) = (name, execute) else {
            return Err(Exception::throw_type(
               &ctx,
               "commands.register({name, execute}) expected, with a name without spaces",
            ));
         };
         ctx.userdata::<Registry<'js>>()
            .ok_or_else(|| Exception::throw_internal(&ctx, "no registry"))?
            .commands
            .borrow_mut()
            .insert(name.clone(), (owner.clone(), execute.clone()));
         patches::add_command(Command {
            name:        name.clone(),
            description: options
               .get::<_, Option<String>>("description")?
               .unwrap_or_default(),
            icon:        options.get("icon")?,
            plugin:      owner.clone(),
         });
         remover(
            &ctx,
            move |ctx| {
               if let Some(registry) = ctx.userdata::<Registry<'js>>() {
                  let mut commands = registry.commands.borrow_mut();
                  if commands
                     .get(&name)
                     .is_some_and(|(_, current)| *current == execute)
                  {
                     commands.remove(&name);
                     patches::remove_command(&name);
                  }
               }
            },
            None,
         )
      })?,
   )?;
   commands.set(
      "list",
      Function::new(ctx.clone(), |ctx: Ctx<'js>| {
         let list = rquickjs::Array::new(ctx.clone())?;
         for (index, command) in patches::commands().into_iter().enumerate() {
            let entry = Object::new(ctx.clone())?;
            entry.set("name", command.name)?;
            entry.set("description", command.description)?;
            entry.set("plugin", command.plugin)?;
            list.set(index, entry)?;
         }
         Ok::<_, rquickjs::Error>(list)
      })?,
   )?;
   Ok(commands)
}

#[cfg(target_os = "android")]
fn on_send<'js>(
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
   patches::set_interceptors(registry.interceptors.borrow().len());
   drop(registry);
   remover(
      ctx,
      move |ctx| {
         if let Some(registry) = ctx.userdata::<Registry<'js>>() {
            let mut interceptors = registry.interceptors.borrow_mut();
            interceptors.retain(|(existing, ..)| *existing != id);
            patches::set_interceptors(interceptors.len());
         }
      },
      None,
   )
}

#[cfg(target_os = "android")]
fn on_emoji_pick<'js>(
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
      .pickers
      .borrow_mut()
      .push((id, plugin.to_owned(), function));
   patches::set_emoji_pickers(registry.pickers.borrow().len());
   drop(registry);
   remover(
      ctx,
      move |ctx| {
         if let Some(registry) = ctx.userdata::<Registry<'js>>() {
            let mut pickers = registry.pickers.borrow_mut();
            pickers.retain(|(existing, ..)| *existing != id);
            patches::set_emoji_pickers(pickers.len());
         }
      },
      None,
   )
}
