use std::{
   borrow::Cow,
   cell::RefCell,
   collections::{
      BTreeMap,
      BTreeSet,
      HashMap,
   },
   fs,
   io::ErrorKind,
   path::PathBuf,
};

use log::Level;
use plumage_sigs::{
   EventId,
   GatewayRegistry,
};
#[cfg(target_os = "android")] use rquickjs::function::Opt;
use rquickjs::{
   CatchResultExt,
   CaughtError,
   Context,
   Ctx,
   Exception,
   Function,
   IntoJs,
   JsLifetime,
   Object,
   Runtime,
   Value,
   function::Rest,
   object::Accessor,
};
use serde_json::value::RawValue;

use crate::{
   Chosen,
   Control,
   Error,
   Header,
   Plugin,
   PluginInfo,
   STATE,
   Setting,
   State,
   gateway,
   io_error,
   local,
};
#[cfg(target_os = "android")]
use crate::{
   discord,
   fetch,
   store,
};

pub struct Engine {
   runtime: Runtime,
   context: Context,
   plugins: BTreeMap<String, Loaded>,
}

struct Loaded {
   source:    String,
   enabled:   bool,
   error:     Option<String>,
   /// Read from `home/plugins`, so the user can delete it.
   installed: bool,
}

type ChosenMap = BTreeMap<String, BTreeMap<String, Chosen>>;

/// Which plugins the user turned off and how they set each plugin's
/// settings, kept in `home/plugins.json`.
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Choices<'a> {
   #[serde(default)]
   disabled: BTreeSet<String>,
   #[serde(default)]
   settings: Cow<'a, ChosenMap>,
}

/// One setting a plugin defined with `plumage.settings.define`.
struct Definition<'js> {
   key:         String,
   title:       String,
   description: Option<String>,
   kind:        Kind<'js>,
}

/// A setting's type with its default, or a button's handler.
enum Kind<'js> {
   Switch(bool),
   Text(String),
   Number(f64),
   Button(Function<'js>),
}

impl Kind<'_> {
   /// The user's choice when it has this kind's type, else the default.
   fn current(&self, chosen: Option<&Chosen>) -> Control {
      match (self, chosen) {
         (Self::Switch(_), Some(Chosen::Switch(on))) => Control::Switch(*on),
         (Self::Switch(default), _) => Control::Switch(*default),
         (Self::Text(_), Some(Chosen::Text(text))) => Control::Text(text.clone()),
         (Self::Text(default), _) => Control::Text(default.clone()),
         (Self::Number(_), Some(Chosen::Number(number))) => Control::Number(*number),
         (Self::Number(default), _) => Control::Number(*default),
         (Self::Button(_), _) => Control::Button,
      }
   }
}

pub enum Before {
   Keep,
   Drop,
   Replace(String),
}

/// What plugins registered. It lives in the runtime's userdata, so callbacks
/// stay JS values owned by the runtime, and the only thing plugins can reach is
/// their own `plumage` object.
struct Registry<'js> {
   gateway: Object<'js>,
   handles: Vec<(EventId, Object<'js>)>,
   hooks:   RefCell<HashMap<EventId, Vec<Hook<'js>>>>,
   defined: RefCell<BTreeMap<String, Vec<Definition<'js>>>>,
   /// Kept across unloads, so a reloaded plugin sees what the user chose.
   chosen:  RefCell<ChosenMap>,
   /// `plumage.onUnload` callbacks, run when the plugin is turned off,
   /// reloaded or removed.
   unloads: RefCell<HashMap<String, Vec<Function<'js>>>>,
   /// The resolve and reject of each promise a worker thread will settle, by
   /// id.
   #[cfg(target_os = "android")]
   pending: RefCell<HashMap<u64, (Function<'js>, Function<'js>)>>,
}

// SAFETY: every field is bound to 'js, and Changed swaps exactly that lifetime.
unsafe impl<'js> JsLifetime<'js> for Registry<'js> {
   type Changed<'to> = Registry<'to>;
}

#[derive(Clone)]
struct Hook<'js> {
   plugin: String,
   before: Option<Function<'js>>,
   after:  Option<Function<'js>>,
}

#[expect(clippy::needless_pass_by_value, reason = "used as a map_err callback")]
fn js_error(err: CaughtError<'_>) -> Error {
   Error::Js(err.to_string())
}

fn throw(ctx: &Ctx<'_>, err: &Error) -> rquickjs::Error {
   Exception::throw_message(ctx, &err.to_string())
}

fn state() -> &'static State {
   STATE
      .get()
      .expect("natives run after start() publishes the state")
}

impl Engine {
   pub fn new(events: &GatewayRegistry) -> Result<Self, Error> {
      let runtime = Runtime::new().map_err(|err| Error::Js(err.to_string()))?;
      runtime.set_max_stack_size(512 * 1024);
      let context = Context::full(&runtime).map_err(|err| Error::Js(err.to_string()))?;
      context.with(|ctx| {
         let registry = registry(&ctx, events).catch(&ctx).map_err(js_error)?;
         ctx.store_userdata(registry)
            .map_err(|err| Error::Js(err.to_string()))?;
         Ok::<_, Error>(())
      })?;
      Ok(Self {
         runtime,
         context,
         plugins: BTreeMap::new(),
      })
   }

   pub fn plugins(&self) -> Vec<PluginInfo> {
      self.context.with(|ctx| {
         let registry = ctx.userdata::<Registry<'_>>();
         let defined = registry.as_ref().map(|registry| registry.defined.borrow());
         let chosen = registry.as_ref().map(|registry| registry.chosen.borrow());
         self
            .plugins
            .iter()
            .map(|(name, loaded)| {
               let settings = defined
                  .as_ref()
                  .and_then(|defined| defined.get(name))
                  .into_iter()
                  .flatten()
                  .map(|definition| {
                     let picked = chosen
                        .as_ref()
                        .and_then(|chosen| chosen.get(name)?.get(&definition.key));
                     Setting {
                        key:         definition.key.clone(),
                        title:       definition.title.clone(),
                        description: definition.description.clone(),
                        control:     definition.kind.current(picked),
                     }
                  })
                  .collect();
               PluginInfo {
                  name: name.clone(),
                  enabled: loaded.enabled,
                  error: loaded.error.clone(),
                  installed: loaded.installed,
                  header: Header::from(loaded.source.as_str()),
                  settings,
               }
            })
            .collect()
      })
   }

   pub fn set_setting(&self, plugin: &str, key: &str, value: Chosen) {
      self.context.with(|ctx| {
         if let Some(registry) = ctx.userdata::<Registry<'_>>() {
            registry
               .chosen
               .borrow_mut()
               .entry(plugin.to_owned())
               .or_default()
               .insert(key.to_owned(), value);
         }
      });
      self.save_choices();
   }

   pub fn press(&self, plugin: &str, key: &str) {
      self.context.with(|ctx| {
         let handler = ctx.userdata::<Registry<'_>>().and_then(|registry| {
            registry
               .defined
               .borrow()
               .get(plugin)?
               .iter()
               .find(|definition| definition.key == key)
               .and_then(|definition| {
                  match &definition.kind {
                     Kind::Button(handler) => Some(handler.clone()),
                     _ => None,
                  }
               })
         });
         if let Some(handler) = handler
            && let Err(err) = handler.call::<_, ()>(()).catch(&ctx)
         {
            log::error!("[{plugin}] {key}: {err}");
         }
      });
      drain(&self.runtime);
   }

   /// Evaluates `name` with a fresh `plumage` object, recording any error.
   fn eval(&mut self, name: &str) {
      let Some(loaded) = self.plugins.get_mut(name) else {
         return;
      };
      let outcome = self.context.with(|ctx| {
         let run = || -> rquickjs::Result<()> {
            let plumage = api(&ctx, name)?;
            let wrapped = format!("(function (plumage) {{\n{}\n}})", loaded.source);
            ctx.eval::<Function<'_>, _>(wrapped)?.call((plumage,))
         };
         run().catch(&ctx).map_err(js_error)
      });
      drain(&self.runtime);
      match outcome {
         Ok(()) => {
            log::info!("loaded plugin {name}");
            loaded.error = None;
         },
         Err(err) => {
            log::error!("plugin {name} failed to load: {err}");
            loaded.error = Some(err.to_string());
         },
      }
   }

   /// Drops every hook `name` registered, and turns off events nobody else
   /// listens to.
   fn unload(&self, name: &str) {
      self.context.with(|ctx| {
         let Some(registry) = ctx.userdata::<Registry<'_>>() else {
            return;
         };
         let cleanups = registry
            .unloads
            .borrow_mut()
            .remove(name)
            .unwrap_or_default();
         for cleanup in cleanups {
            if let Err(err) = cleanup.call::<_, ()>(()).catch(&ctx) {
               log::error!("[{name}] onUnload: {err}");
            }
         }
         registry.defined.borrow_mut().remove(name);
         let mut hooks = registry.hooks.borrow_mut();
         for (event, list) in hooks.iter_mut() {
            list.retain(|hook| hook.plugin != name);
            if list.is_empty() {
               gateway::uninstall(*event);
            }
         }
         hooks.retain(|_, list| !list.is_empty());
      });
      drain(&self.runtime);
   }

   pub fn set_enabled(&mut self, name: &str, enabled: bool) {
      let Some(loaded) = self.plugins.get_mut(name) else {
         return;
      };
      if loaded.enabled == enabled {
         return;
      }
      loaded.enabled = enabled;
      loaded.error = None;
      if enabled {
         self.eval(name);
      } else {
         self.unload(name);
      }
      self.save_choices();
   }

   /// Adds `name` or replaces its source, then runs it unless the user turned
   /// it off.
   #[cfg(target_os = "android")]
   pub fn install(&mut self, name: &str, source: String) {
      let enabled = if let Some(loaded) = self.plugins.get_mut(name) {
         loaded.source = source;
         loaded.installed = true;
         loaded.enabled
      } else {
         self.plugins.insert(name.to_owned(), Loaded {
            source,
            enabled: true,
            error: None,
            installed: true,
         });
         true
      };
      if enabled {
         self.unload(name);
         self.eval(name);
      }
   }

   /// Drops `name` and what the user chose for it. A bundled plugin it
   /// replaced comes back at the next start.
   #[cfg(target_os = "android")]
   pub fn uninstall(&mut self, name: &str) {
      self.unload(name);
      self.plugins.remove(name);
      self.context.with(|ctx| {
         if let Some(registry) = ctx.userdata::<Registry<'_>>() {
            registry.chosen.borrow_mut().remove(name);
         }
      });
      self.save_choices();
   }

   pub fn reload(&mut self, name: &str) {
      if self.plugins.get(name).is_some_and(|loaded| loaded.enabled) {
         self.unload(name);
         self.eval(name);
      }
   }

   fn save_choices(&self) {
      let disabled = self
         .plugins
         .iter()
         .filter(|(_, loaded)| !loaded.enabled)
         .map(|(name, _)| name.clone())
         .collect();
      let json = self.context.with(|ctx| {
         let registry = ctx.userdata::<Registry<'_>>();
         let chosen = registry.as_ref().map(|registry| registry.chosen.borrow());
         let empty = BTreeMap::new();
         serde_json::to_vec(&Choices {
            disabled,
            settings: Cow::Borrowed(chosen.as_deref().unwrap_or(&empty)),
         })
      });
      let path = state().choices_path();
      let write = || -> std::io::Result<()> {
         fs::create_dir_all(path.parent().expect("the choices path has a parent"))?;
         fs::write(&path, json?)
      };
      if let Err(err) = write() {
         log::error!("{}: {err}", path.display());
      }
   }

   /// The result as JSON when it has a JSON form, else as `String(result)`.
   /// The prompt sees a `plumage` object of its own, named "console".
   pub fn evaluate(&self, code: &str) -> Result<String, Error> {
      let result = self.context.with(|ctx| {
         let run = || -> rquickjs::Result<String> {
            if !ctx.globals().contains_key("plumage")? {
               ctx.globals().set("plumage", api(&ctx, "console")?)?;
            }
            let result = ctx.eval::<Value<'_>, _>(code)?;
            if let Some(text) = result.as_string() {
               return text.to_string();
            }
            match ctx.json_stringify(result.clone())? {
               Some(json) => json.to_string(),
               None => {
                  ctx.globals()
                     .get::<_, Function<'_>>("String")?
                     .call((result,))
               },
            }
         };
         run().catch(&ctx).map_err(js_error)
      });
      drain(&self.runtime);
      result
   }

   /// Runs the `before` hooks for `event`. Each sees the previous hook's
   /// payload.
   pub fn before(&self, event: EventId, name: &str, json: &str) -> Result<Before, Error> {
      let result = self.context.with(|ctx| {
         let hooks = registered(&ctx, event, |hook| hook.before.as_ref());
         if hooks.is_empty() {
            return Ok(Before::Keep);
         }
         let mut payload = ctx.json_parse(json).catch(&ctx).map_err(js_error)?;
         for (plugin, before) in hooks {
            match before.call::<_, Value<'_>>((payload.clone(),)).catch(&ctx) {
               Ok(result) if result.as_bool() == Some(false) => return Ok(Before::Drop),
               Ok(result) if result.is_undefined() => {},
               Ok(result) => payload = result,
               Err(err) => log::error!("[{plugin}] before {name}: {err}"),
            }
         }
         let json = ctx
            .json_stringify(payload)
            .catch(&ctx)
            .map_err(js_error)?
            .ok_or_else(|| Error::Js(format!("a before {name} hook returned no JSON value")))?;
         Ok(Before::Replace(
            json.to_string().map_err(|err| Error::Js(err.to_string()))?,
         ))
      });
      drain(&self.runtime);
      result
   }

   pub fn after(&self, event: EventId, name: &str, json: &str, ok: bool) -> Result<(), Error> {
      let result = self.context.with(|ctx| {
         let hooks = registered(&ctx, event, |hook| hook.after.as_ref());
         if hooks.is_empty() {
            return Ok(());
         }
         let payload = ctx.json_parse(json).catch(&ctx).map_err(js_error)?;
         for (plugin, after) in hooks {
            if let Err(err) = after.call::<_, ()>((payload.clone(), ok)).catch(&ctx) {
               log::error!("[{plugin}] after {name}: {err}");
            }
         }
         Ok(())
      });
      drain(&self.runtime);
      result
   }

   /// Settles promise `id` with what its worker thread produced, and runs what
   /// awaited it.
   #[cfg(target_os = "android")]
   pub fn settle(&self, id: u64, settlement: Settlement) {
      self.context.with(|ctx| {
         let Some((resolve, reject)) = ctx
            .userdata::<Registry<'_>>()
            .and_then(|registry| registry.pending.borrow_mut().remove(&id))
         else {
            return;
         };
         let value: rquickjs::Result<Result<Value<'_>, String>> = match settlement {
            Settlement::Fetch(Ok(response)) | Settlement::Discord(Ok(response)) => {
               response_object(&ctx, &response).map(|object| Ok(object.into_value()))
            },
            Settlement::Rows(Ok(rows)) => {
               serde_json::to_string(&rows)
                  .map_err(|err| Exception::throw_internal(&ctx, &err.to_string()))
                  .and_then(|json| ctx.json_parse(json))
                  .map(Ok)
            },
            Settlement::Fetch(Err(err)) => Ok(Err(err.to_string())),
            Settlement::Discord(Err(err)) => Ok(Err(err.to_string())),
            Settlement::Rows(Err(err)) => Ok(Err(err.to_string())),
         };
         let outcome = value.and_then(|value| {
            match value {
               Ok(value) => resolve.call::<_, ()>((value,)),
               Err(message) => {
                  reject.call::<_, ()>((Exception::from_message(ctx.clone(), &message)?,))
               },
            }
         });
         if let Err(err) = outcome.catch(&ctx) {
            log::error!("settling promise {id}: {err}");
         }
      });
      drain(&self.runtime);
   }
}

/// Runs promise reactions until none are left. `QuickJS` queues them, and
/// nothing else runs the queue.
fn drain(runtime: &Runtime) {
   loop {
      match runtime.execute_pending_job() {
         Ok(true) => {},
         Ok(false) => break,
         Err(err) => log::error!("a promise job threw: {err}"),
      }
   }
}

/// `{status, ok, text(), json()}`, a subset of the web's `Response`.
#[cfg(target_os = "android")]
fn response_object<'js>(
   ctx: &Ctx<'js>,
   response: &fetch::Response,
) -> rquickjs::Result<Object<'js>> {
   let object = Object::new(ctx.clone())?;
   object.set("status", response.status)?;
   object.set("ok", (200..300).contains(&response.status))?;
   let text = String::from_utf8_lossy(&response.body).into_owned();
   let json_text = text.clone();
   object.set("text", Function::new(ctx.clone(), move || text.clone())?)?;
   object.set(
      "json",
      Function::new(ctx.clone(), move |ctx: Ctx<'js>| {
         ctx.json_parse(json_text.clone())
      })?,
   )?;
   Ok(object)
}

/// Snapshot of one phase's callbacks, so a callback that registers a hook does
/// not run into the borrow held while iterating.
fn registered<'js>(
   ctx: &Ctx<'js>,
   event: EventId,
   phase: impl for<'hook> Fn(&'hook Hook<'js>) -> Option<&'hook Function<'js>>,
) -> Vec<(String, Function<'js>)> {
   let Some(registry) = ctx.userdata::<Registry<'js>>() else {
      return Vec::new();
   };
   let hooks = registry.hooks.borrow();
   hooks
      .get(&event)
      .into_iter()
      .flatten()
      .filter_map(|hook| Some((hook.plugin.clone(), phase(hook)?.clone())))
      .collect()
}

/// One frozen handle per live registry entry. Plugins compare nothing and parse
/// nothing, the runtime recognizes a handle by identity.
fn registry<'js>(ctx: &Ctx<'js>, events: &GatewayRegistry) -> rquickjs::Result<Registry<'js>> {
   let freeze = ctx
      .globals()
      .get::<_, Object>("Object")?
      .get::<_, Function>("freeze")?;
   let gateway = Object::new(ctx.clone())?;
   let mut handles = Vec::new();
   for (event, entry) in events.iter() {
      let key = camel_case(&entry.name);
      let handle = Object::new(ctx.clone())?;
      let label = format!("gateway.{key}");
      handle.set(
         "toString",
         Function::new(ctx.clone(), move || label.clone())?,
      )?;
      let handle = freeze.call::<_, Object<'js>>((handle,))?;
      gateway.set(key, handle.clone())?;
      handles.push((event, handle));
   }
   let gateway = freeze.call((gateway,))?;
   Ok(Registry {
      gateway,
      handles,
      hooks: RefCell::default(),
      defined: RefCell::default(),
      chosen: RefCell::default(),
      unloads: RefCell::default(),
      #[cfg(target_os = "android")]
      pending: RefCell::default(),
   })
}

/// `MESSAGE_CREATE` to `messageCreate`, the key plugins and `plumage.d.ts` use.
fn camel_case(name: &str) -> String {
   name
      .split('_')
      .enumerate()
      .map(|(index, word)| {
         let lower = word.to_ascii_lowercase();
         let mut chars = lower.chars();
         match chars.next() {
            Some(first) if index > 0 => first.to_ascii_uppercase().to_string() + chars.as_str(),
            _ => lower,
         }
      })
      .collect()
}

fn event_of<'js>(ctx: &Ctx<'js>, sig: &Value<'js>) -> rquickjs::Result<EventId> {
   ctx.userdata::<Registry<'js>>()
      .ok_or_else(|| Exception::throw_internal(ctx, "no registry"))?
      .handles
      .iter()
      .find(|(_, handle)| handle.as_value() == sig)
      .map(|(event, _)| *event)
      .ok_or_else(|| Exception::throw_type(ctx, "expected a hook point from plumage.gateway"))
}

fn register<'js>(
   ctx: &Ctx<'js>,
   plugin: &str,
   sig: &Value<'js>,
   before: Option<Function<'js>>,
   after: Option<Function<'js>>,
) -> rquickjs::Result<()> {
   let event = event_of(ctx, sig)?;
   gateway::install(event);
   ctx.userdata::<Registry<'js>>()
      .ok_or_else(|| Exception::throw_internal(ctx, "no registry"))?
      .hooks
      .borrow_mut()
      .entry(event)
      .or_default()
      .push(Hook {
         plugin: plugin.to_owned(),
         before,
         after,
      });
   Ok(())
}

/// The `plumage` object one plugin receives.
fn api<'js>(ctx: &Ctx<'js>, plugin: &str) -> rquickjs::Result<Object<'js>> {
   let api = Object::new(ctx.clone())?;
   let gateway = ctx
      .userdata::<Registry<'js>>()
      .ok_or_else(|| Exception::throw_internal(ctx, "no registry"))?;
   api.set("gateway", gateway.gateway.clone())?;
   drop(gateway);

   let name = plugin.to_owned();
   api.set(
      "on",
      Function::new(
         ctx.clone(),
         move |ctx: Ctx<'js>, sig: Value<'js>, listener: Function<'js>| {
            register(&ctx, &name, &sig, None, Some(listener))
         },
      )?,
   )?;
   let name = plugin.to_owned();
   api.set(
      "hook",
      Function::new(
         ctx.clone(),
         move |ctx: Ctx<'js>, sig: Value<'js>, hooks: Object<'js>| {
            register(&ctx, &name, &sig, hooks.get("before")?, hooks.get("after")?)
         },
      )?,
   )?;
   api.set(
      "dispatch",
      Function::new(
         ctx.clone(),
         |ctx: Ctx<'js>, sig: Value<'js>, payload: Value<'js>| {
            let event = event_of(&ctx, &sig)?;
            let json = ctx
               .json_stringify(payload)?
               .ok_or_else(|| Exception::throw_type(&ctx, "dispatch needs a JSON payload"))?
               .to_string()?;
            gateway::dispatch(event, &json).ok_or_else(|| {
               Exception::throw_message(&ctx, "dispatch runs only inside a gateway hook")
            })
         },
      )?,
   )?;

   api.set(
      "enqueue",
      Function::new(
         ctx.clone(),
         |ctx: Ctx<'js>, sig: Value<'js>, payload: Value<'js>| {
            let event = event_of(&ctx, &sig)?;
            let json = ctx
               .json_stringify(payload)?
               .ok_or_else(|| Exception::throw_type(&ctx, "enqueue needs a JSON payload"))?
               .to_string()?;
            gateway::enqueue(event, json);
            Ok::<_, rquickjs::Error>(())
         },
      )?,
   )?;

   api.set("messages", messages_object(ctx)?)?;
   api.set("log", log_object(ctx, plugin)?)?;
   api.set("storage", storage(ctx, plugin)?)?;
   let settings = Object::new(ctx.clone())?;
   let name = plugin.to_owned();
   settings.set(
      "define",
      Function::new(ctx.clone(), move |ctx: Ctx<'js>, schema: Object<'js>| {
         define(&ctx, &name, &schema)
      })?,
   )?;
   api.set("settings", settings)?;
   let name = plugin.to_owned();
   api.set(
      "onUnload",
      Function::new(ctx.clone(), move |ctx: Ctx<'js>, cleanup: Function<'js>| {
         ctx.userdata::<Registry<'js>>()
            .ok_or_else(|| Exception::throw_internal(&ctx, "no registry"))?
            .unloads
            .borrow_mut()
            .entry(name.clone())
            .or_default()
            .push(cleanup);
         Ok::<_, rquickjs::Error>(())
      })?,
   )?;
   #[cfg(target_os = "android")]
   api.set("stores", stores_object(ctx)?)?;
   #[cfg(target_os = "android")]
   api.set("discord", discord_object(ctx)?)?;
   #[cfg(target_os = "android")]
   api.set(
      "fetch",
      Function::new(
         ctx.clone(),
         |ctx: Ctx<'js>, url: String, init: Opt<Object<'js>>| start_fetch(&ctx, url, init.0),
      )?,
   )?;
   Ok(api)
}

/// `plumage.messages`, local-only messages.
fn messages_object<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Object<'js>> {
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
               Some(embeds) => ctx.json_stringify(embeds)?.map(|json| json.to_string()).transpose()?,
               None => None,
            };
            let embeds = RawValue::from_string(embeds.unwrap_or_else(|| "[]".to_owned()))
               .map_err(|err| Exception::throw_type(&ctx, &format!("embeds: {err}")))?;
            let options = local::Options {
               author:   author.as_deref().unwrap_or("Plumage"),
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

/// `plumage.log`, whose lines carry the plugin's name.
fn log_object<'js>(ctx: &Ctx<'js>, plugin: &str) -> rquickjs::Result<Object<'js>> {
   let log = Object::new(ctx.clone())?;
   for (method, level) in [
      ("debug", Level::Debug),
      ("info", Level::Info),
      ("warn", Level::Warn),
      ("error", Level::Error),
   ] {
      let name = plugin.to_owned();
      log.set(
         method,
         Function::new(ctx.clone(), move |ctx: Ctx<'js>, args: Rest<Value<'js>>| {
            let parts = args
               .0
               .into_iter()
               .map(|arg| {
                  if let Some(text) = arg.as_string() {
                     return text.to_string();
                  }
                  ctx.json_stringify(arg)?
                     .map_or_else(|| Ok("undefined".to_owned()), |json| json.to_string())
               })
               .collect::<rquickjs::Result<Vec<_>>>()?;
            log::log!(level, "[{name}] {}", parts.join(" "));
            Ok::<_, rquickjs::Error>(())
         })?,
      )?;
   }
   Ok(log)
}

/// A fetch-style `{method, headers, body}`. A body that is not a string is
/// sent as JSON.
#[cfg(target_os = "android")]
struct Init {
   method:  String,
   headers: Vec<(String, String)>,
   body:    Option<Vec<u8>>,
}

#[cfg(target_os = "android")]
impl Init {
   fn read<'js>(ctx: &Ctx<'js>, init: Option<Object<'js>>) -> rquickjs::Result<Self> {
      let mut out = Self {
         method:  "GET".to_owned(),
         headers: Vec::new(),
         body:    None,
      };
      let Some(init) = init else {
         return Ok(out);
      };
      if let Some(method) = init.get::<_, Option<String>>("method")? {
         out.method = method.to_ascii_uppercase();
      }
      if let Some(headers) = init.get::<_, Option<Object<'js>>>("headers")? {
         out.headers = headers
            .props::<String, String>()
            .collect::<rquickjs::Result<_>>()?;
      }
      out.set_body(ctx, init.get("body")?)?;
      Ok(out)
   }

   fn set_body<'js>(&mut self, ctx: &Ctx<'js>, body: Value<'js>) -> rquickjs::Result<()> {
      if body.is_undefined() || body.is_null() {
         return Ok(());
      }
      if let Some(text) = body.as_string() {
         self.body = Some(text.to_string()?.into_bytes());
         return Ok(());
      }
      if !self
         .headers
         .iter()
         .any(|(name, _)| name.eq_ignore_ascii_case("content-type"))
      {
         self
            .headers
            .push(("Content-Type".to_owned(), "application/json".to_owned()));
      }
      self.body = ctx
         .json_stringify(body)?
         .map(|json| json.to_string())
         .transpose()?
         .map(String::into_bytes);
      Ok(())
   }
}

/// Sends the request on its own thread, so neither the gateway nor the UI
/// waits on the network, and returns a promise that settles when it answers.
#[cfg(target_os = "android")]
fn start_fetch<'js>(
   ctx: &Ctx<'js>,
   url: String,
   init: Option<Object<'js>>,
) -> rquickjs::Result<rquickjs::Promise<'js>> {
   let Init {
      method,
      headers,
      body,
   } = Init::read(ctx, init)?;
   let request = fetch::Request {
      url,
      method,
      headers,
      body,
   };
   in_background(ctx, move || Settlement::Fetch(fetch::send(&request)))
}

/// `plumage.discord`, Discord's API on the app's own client and session:
/// `request(path, init)`, `get` and `delete(path, init)`, and `post`, `put` and
/// `patch(path, body, init)`.
#[cfg(target_os = "android")]
fn discord_object<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Object<'js>> {
   fn send<'js>(
      ctx: &Ctx<'js>,
      path: String,
      init: Init,
   ) -> rquickjs::Result<rquickjs::Promise<'js>> {
      in_background(ctx, move || {
         Settlement::Discord(discord::send(
            &init.method,
            &path,
            init.headers,
            init.body.unwrap_or_default(),
         ))
      })
   }

   let object = Object::new(ctx.clone())?;
   object.set(
      "request",
      Function::new(
         ctx.clone(),
         |ctx: Ctx<'js>, path: String, init: Opt<Object<'js>>| {
            send(&ctx, path, Init::read(&ctx, init.0)?)
         },
      )?,
   )?;
   for verb in ["GET", "DELETE"] {
      object.set(
         verb.to_ascii_lowercase(),
         Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, path: String, init: Opt<Object<'js>>| {
               let mut init = Init::read(&ctx, init.0)?;
               verb.clone_into(&mut init.method);
               send(&ctx, path, init)
            },
         )?,
      )?;
   }
   for verb in ["POST", "PUT", "PATCH"] {
      object.set(
         verb.to_ascii_lowercase(),
         Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, path: String, body: Opt<Value<'js>>, init: Opt<Object<'js>>| {
               let mut init = Init::read(&ctx, init.0)?;
               verb.clone_into(&mut init.method);
               if let Some(body) = body.0 {
                  init.set_body(&ctx, body)?;
               }
               send(&ctx, path, init)
            },
         )?,
      )?;
   }
   Ok(object)
}

/// What a worker thread hands back to settle a promise with.
#[cfg(target_os = "android")]
pub enum Settlement {
   Fetch(Result<fetch::Response, fetch::FetchError>),
   Discord(Result<fetch::Response, discord::DiscordError>),
   Rows(Result<Vec<serde_json::Map<String, serde_json::Value>>, store::StoreError>),
}

/// Runs `work` on its own thread and returns a promise it settles. The
/// resolvers stay in the registry, since JS values cannot leave the thread
/// that holds the engine.
#[cfg(target_os = "android")]
fn in_background<'js>(
   ctx: &Ctx<'js>,
   work: impl FnOnce() -> Settlement + Send + 'static,
) -> rquickjs::Result<rquickjs::Promise<'js>> {
   static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

   let (promise, resolve, reject) = ctx.promise()?;
   let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
   ctx.userdata::<Registry<'js>>()
      .ok_or_else(|| Exception::throw_internal(ctx, "no registry"))?
      .pending
      .borrow_mut()
      .insert(id, (resolve, reject));
   std::thread::spawn(move || {
      let settlement = work();
      if let Some(engine) = crate::engine() {
         engine.settle(id, settlement);
      }
   });
   Ok(promise)
}

/// `plumage.stores`, read-only SQL over Phoenix's cache.
#[cfg(target_os = "android")]
fn stores_object<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Object<'js>> {
   let stores = Object::new(ctx.clone())?;
   stores.set(
      "sql",
      Function::new(
         ctx.clone(),
         |ctx: Ctx<'js>, sql: String, args: Rest<Value<'js>>| {
            let args = args
               .0
               .into_iter()
               .map(|arg| {
                  if let Some(text) = arg.as_string() {
                     return text.to_string();
                  }
                  ctx.json_stringify(arg)?
                     .map_or_else(|| Ok("null".to_owned()), |json| json.to_string())
               })
               .collect::<rquickjs::Result<Vec<String>>>()?;
            let files = state().files_dir();
            in_background(&ctx, move || {
               Settlement::Rows(
                  store::database(&files).and_then(|db| store::query(&db, &sql, &args)),
               )
            })
         },
      )?,
   )?;
   Ok(stores)
}

/// Records `schema` as the plugin's settings and returns an object with one
/// getter per non-button key that reads the user's current choice.
fn define<'js>(
   ctx: &Ctx<'js>,
   plugin: &str,
   schema: &Object<'js>,
) -> rquickjs::Result<Object<'js>> {
   let mut definitions = Vec::new();
   for entry in schema.props::<String, Object<'js>>() {
      let (key, spec) = entry?;
      let kind = match spec.get::<_, String>("type")?.as_str() {
         "switch" => Kind::Switch(spec.get::<_, Option<bool>>("default")?.unwrap_or_default()),
         "text" => {
            Kind::Text(
               spec
                  .get::<_, Option<String>>("default")?
                  .unwrap_or_default(),
            )
         },
         "number" => Kind::Number(spec.get::<_, Option<f64>>("default")?.unwrap_or_default()),
         "button" => Kind::Button(spec.get("onPress")?),
         other => {
            return Err(Exception::throw_type(
               ctx,
               &format!(
                  "setting {key}: unknown type {other:?}, expected switch, text, number or button"
               ),
            ));
         },
      };
      definitions.push(Definition {
         title: spec.get("title")?,
         description: spec.get("description")?,
         kind,
         key,
      });
   }

   let values = Object::new(ctx.clone())?;
   for definition in &definitions {
      if matches!(definition.kind, Kind::Button(_)) {
         continue;
      }
      let (plugin, key) = (plugin.to_owned(), definition.key.clone());
      let read = move |ctx: Ctx<'js>| -> rquickjs::Result<Value<'js>> {
         let registry = ctx
            .userdata::<Registry<'js>>()
            .ok_or_else(|| Exception::throw_internal(&ctx, "no registry"))?;
         let defined = registry.defined.borrow();
         let chosen = registry.chosen.borrow();
         let Some(definition) = defined
            .get(&plugin)
            .and_then(|list| list.iter().find(|definition| definition.key == key))
         else {
            return Ok(Value::new_undefined(ctx.clone()));
         };
         let picked = chosen.get(&plugin).and_then(|values| values.get(&key));
         match definition.kind.current(picked) {
            Control::Switch(on) => on.into_js(&ctx),
            Control::Text(text) => text.into_js(&ctx),
            Control::Number(number) => number.into_js(&ctx),
            Control::Button => Ok(Value::new_undefined(ctx.clone())),
         }
      };
      values.prop(
         definition.key.as_str(),
         Accessor::new_get(read).enumerable(),
      )?;
   }
   ctx.userdata::<Registry<'js>>()
      .ok_or_else(|| Exception::throw_internal(ctx, "no registry"))?
      .defined
      .borrow_mut()
      .insert(plugin.to_owned(), definitions);
   Ok(values)
}

/// A JSON object per plugin, loaded once and rewritten on every change.
fn storage<'js>(ctx: &Ctx<'js>, plugin: &str) -> rquickjs::Result<Object<'js>> {
   let path = state().storage_path(plugin);
   let text = match fs::read_to_string(&path) {
      Ok(text) => text,
      Err(err) if err.kind() == ErrorKind::NotFound => "{}".to_owned(),
      Err(err) => return Err(throw(ctx, &io_error(&path)(err))),
   };
   let store = ctx.json_parse(text)?.into_object().ok_or_else(|| {
      Exception::throw_type(ctx, &format!("{} is not a JSON object", path.display()))
   })?;

   let persist = move |ctx: &Ctx<'js>, store: &Object<'js>, path: &PathBuf| {
      let json = ctx
         .json_stringify(store.clone())?
         .map(|json| json.to_string())
         .transpose()?;
      let write = || {
         fs::create_dir_all(path.parent().expect("storage paths have a parent"))?;
         fs::write(path, json.unwrap_or_default())
      };
      write().map_err(|err| throw(ctx, &io_error(path)(err)))
   };

   let storage = Object::new(ctx.clone())?;
   let read = store.clone();
   storage.set(
      "get",
      Function::new(ctx.clone(), move |key: String| {
         read.get::<_, Value<'js>>(key)
      })?,
   )?;
   let (write, file) = (store.clone(), path.clone());
   storage.set(
      "set",
      Function::new(
         ctx.clone(),
         move |ctx: Ctx<'js>, key: String, value: Value<'js>| {
            write.set(key, value)?;
            persist(&ctx, &write, &file)
         },
      )?,
   )?;
   let (write, file) = (store.clone(), path);
   storage.set(
      "delete",
      Function::new(ctx.clone(), move |ctx: Ctx<'js>, key: String| {
         write.remove(key)?;
         persist(&ctx, &write, &file)
      })?,
   )?;
   storage.set(
      "keys",
      Function::new(ctx.clone(), move || {
         store.keys::<String>().collect::<rquickjs::Result<Vec<_>>>()
      })?,
   )?;
   Ok(storage)
}

/// Evaluates `bundled` plus every `*.js` file in the plugins directory, in name
/// order, skipping the ones the user turned off.
#[expect(
   clippy::significant_drop_tightening,
   reason = "plugins load under one engine lock so no gateway event sees a half-loaded set"
)]
pub fn load_plugins(state: &State, bundled: Vec<Plugin>) -> Result<Vec<String>, Error> {
   let dir = state.plugins_dir();
   let mut sources = bundled
      .into_iter()
      .map(|plugin| (plugin.name, (plugin.source, false)))
      .collect::<BTreeMap<String, (String, bool)>>();
   let paths = match fs::read_dir(&dir) {
      Ok(entries) => {
         entries
            .collect::<Result<Vec<_>, _>>()
            .map_err(io_error(&dir))?
      },
      Err(err) if err.kind() == ErrorKind::NotFound => Vec::new(),
      Err(err) => return Err(io_error(&dir)(err)),
   };
   for path in paths.iter().map(fs::DirEntry::path) {
      let Some(name) = path
         .file_stem()
         .and_then(|stem| stem.to_str())
         .filter(|_| path.extension().is_some_and(|ext| ext == "js"))
      else {
         continue;
      };
      let source = fs::read_to_string(&path).map_err(io_error(&path))?;
      sources.insert(name.to_owned(), (source, true));
   }

   let choices_path = state.choices_path();
   let choices = match fs::read(&choices_path) {
      Ok(bytes) => {
         serde_json::from_slice::<Choices>(&bytes).unwrap_or_else(|err| {
            log::error!("{}: {err}", choices_path.display());
            Choices::default()
         })
      },
      Err(err) if err.kind() == ErrorKind::NotFound => Choices::default(),
      Err(err) => return Err(io_error(&choices_path)(err)),
   };

   let mut engine = state
      .js
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
   engine.context.with(|ctx| {
      if let Some(registry) = ctx.userdata::<Registry<'_>>() {
         *registry.chosen.borrow_mut() = choices.settings.into_owned();
      }
   });
   engine.plugins = sources
      .into_iter()
      .map(|(name, (source, installed))| {
         let enabled = !choices.disabled.contains(&name);
         (name, Loaded {
            source,
            enabled,
            error: None,
            installed,
         })
      })
      .collect();
   let enabled = engine
      .plugins
      .iter()
      .filter(|(_, loaded)| loaded.enabled)
      .map(|(name, _)| name.clone())
      .collect::<Vec<String>>();
   for name in &enabled {
      engine.eval(name);
   }
   Ok(enabled
      .into_iter()
      .filter(|name| {
         engine
            .plugins
            .get(name)
            .is_some_and(|loaded| loaded.error.is_none())
      })
      .collect())
}
