use std::{
   cell::RefCell,
   collections::HashMap,
   fs,
   io::ErrorKind,
   path::PathBuf,
};

use log::Level;
use plumage_sigs::{
   EventId,
   GatewayRegistry,
};
use rquickjs::{
   CatchResultExt,
   CaughtError,
   Context,
   Ctx,
   Exception,
   Function,
   JsLifetime,
   Object,
   Runtime,
   Value,
   function::Rest,
};

use crate::{
   Error,
   STATE,
   State,
   gateway,
   io_error,
};

pub struct Engine {
   _runtime: Runtime,
   context:  Context,
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
         _runtime: runtime,
         context,
      })
   }

   /// Runs the `before` hooks for `event`. Each sees the previous hook's
   /// payload.
   pub fn before(&self, event: EventId, name: &str, json: &str) -> Result<Before, Error> {
      self.context.with(|ctx| {
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
      })
   }

   pub fn after(&self, event: EventId, name: &str, json: &str, ok: bool) -> Result<(), Error> {
      self.context.with(|ctx| {
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
      })
   }
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
   let freeze: Function<'js> = ctx
      .globals()
      .get::<_, Object<'js>>("Object")?
      .get("freeze")?;
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

fn register<'js>(
   ctx: &Ctx<'js>,
   plugin: &str,
   sig: &Value<'js>,
   before: Option<Function<'js>>,
   after: Option<Function<'js>>,
) -> rquickjs::Result<()> {
   let registry = ctx
      .userdata::<Registry<'js>>()
      .ok_or_else(|| Exception::throw_internal(ctx, "no registry"))?;
   let event = registry
      .handles
      .iter()
      .find(|(_, handle)| handle.as_value() == sig)
      .map(|(event, _)| *event)
      .ok_or_else(|| Exception::throw_type(ctx, "expected a hook point from plumage.gateway"))?;
   gateway::install(state(), event).map_err(|err| throw(ctx, &err))?;
   registry
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
   api.set("log", log)?;
   api.set("storage", storage(ctx, plugin)?)?;
   Ok(api)
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

/// Evaluates every `*.js` bundle in the plugins directory, in file name order.
#[expect(
   clippy::significant_drop_tightening,
   reason = "plugins load under one engine lock so no gateway event sees a half-loaded set"
)]
pub fn load_plugins(state: &State) -> Result<Vec<String>, Error> {
   let dir = state.plugins_dir();
   let mut paths: Vec<_> = match fs::read_dir(&dir) {
      Ok(entries) => {
         entries
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<Result<_, _>>()
            .map_err(io_error(&dir))?
      },
      Err(err) if err.kind() == ErrorKind::NotFound => Vec::new(),
      Err(err) => return Err(io_error(&dir)(err)),
   };
   paths.retain(|path| path.extension().is_some_and(|ext| ext == "js"));
   paths.sort();

   let engine = state
      .js
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
   let mut loaded = Vec::new();
   for path in paths {
      let name = path
         .file_stem()
         .and_then(|stem| stem.to_str())
         .unwrap_or("plugin")
         .to_owned();
      let source = fs::read_to_string(&path).map_err(io_error(&path))?;
      let outcome = engine.context.with(|ctx| {
         let run = || -> rquickjs::Result<()> {
            let plumage = api(&ctx, &name)?;
            let wrapped = format!("(function (plumage) {{\n{source}\n}})");
            ctx.eval::<Function<'_>, _>(wrapped)?.call((plumage,))
         };
         run().catch(&ctx).map_err(js_error)
      });
      match outcome {
         Ok(()) => {
            log::info!("loaded plugin {name}");
            loaded.push(name);
         },
         Err(err) => log::error!("plugin {name} failed to load: {err}"),
      }
   }
   Ok(loaded)
}
