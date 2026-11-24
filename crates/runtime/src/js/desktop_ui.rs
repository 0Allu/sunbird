//! Desktop `phoenix.ui` bindings backed by [`phoenix_ui_desktop`].
//!
//! Native clipboard, toasts, and notifications run through `phoenix-ui-desktop`.
//! Mobile-only UI-op slot hooks (`addComposerButton`, `addMessageAccessory`,
//! etc.) return a harmless no-op remover so cross-platform plugins load cleanly
//! on desktop without throwing.

use rquickjs::{
   Ctx,
   Exception,
   Function,
   Object,
   Value,
   function::Opt,
};

fn noop_handle<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Function<'js>> {
   let remove = Function::new(ctx.clone(), || {})?;
   remove.set("update", Function::new(ctx.clone(), || {})?)?;
   Ok(remove)
}

pub fn desktop_ui_object<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Object<'js>> {
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

   // Graceful no-op shims for mobile UI-op surfaces so cross-platform plugins
   // (e.g. spoiler-mode, message-logger) load without throwing on desktop.
   for slot_fn in [
      "addHeaderButton",
      "addComposerButton",
      "addMessageAccessory",
      "addMessageAction",
      "addProfileSection",
      "addProfileTab",
      "hide",
      "patchProps",
      "onEvent",
      "onNode",
      "mount",
   ] {
      ui.set(
         slot_fn,
         Function::new(ctx.clone(), |ctx: Ctx<'js>| noop_handle(&ctx))?,
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
            let err = Exception::from_message(
               ctx,
               "phoenix.ui.choose is not yet implemented on desktop",
            )?;
            reject.call::<_, ()>((err,))?;
            Ok(promise)
         },
      )?,
   )?;
   Ok(ui)
}
