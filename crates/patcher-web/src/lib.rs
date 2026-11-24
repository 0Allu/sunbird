//! The patcher for the site, run in a worker. Each split is copied into wasm
//! memory once, and the APK comes back as views of that memory wherever it
//! reuses input bytes, so the page must turn them into a `Blob` before calling
//! in again.

use std::{
   borrow::Cow,
   collections::BTreeMap,
};

use js_sys::{
   Array,
   Function,
   Uint8Array,
};
use patcher::{
   Frida,
   Library,
   PackageName,
   Payload,
   Prepared,
   Runtime,
};
use wasm_bindgen::{
   JsError,
   JsValue,
   prelude::wasm_bindgen,
};

#[wasm_bindgen]
#[derive(Default)]
pub struct Build {
   apks:      Vec<Vec<u8>>,
   libraries: Vec<Library>,
   plugins:   BTreeMap<String, String>,
   scripts:   Vec<(String, String)>,
   frida:     Option<Frida>,
   prepared:  Option<Prepared>,
}

#[wasm_bindgen]
impl Build {
   #[wasm_bindgen(constructor)]
   #[must_use]
   pub fn new() -> Self {
      Self::default()
   }

   /// A split of any kind, told apart by its contents.
   #[wasm_bindgen(js_name = addApk)]
   pub fn add_apk(&mut self, bytes: Vec<u8>) {
      self.apks.push(bytes);
   }

   /// A library for the plugin runtime, the loader under `patcher::LOADER`.
   /// Without any, only the access patches go in.
   #[wasm_bindgen(js_name = addLibrary)]
   pub fn add_library(&mut self, name: String, bytes: Vec<u8>) -> Result<(), JsError> {
      self.libraries.push(Library::new(name, bytes)?);
      Ok(())
   }

   #[wasm_bindgen(js_name = addPlugin)]
   pub fn add_plugin(&mut self, name: String, source: String) {
      self.plugins.insert(name, source);
   }

   #[wasm_bindgen(js_name = addFridaScript)]
   pub fn add_frida_script(&mut self, name: String, code: String) {
      self.scripts.push((name, code));
   }

   /// Embeds frida-gadget, gzipped or not, running `bootstrap` with the
   /// scripts added so far.
   #[wasm_bindgen(js_name = setFrida)]
   pub fn set_frida(&mut self, gadget: Vec<u8>, bootstrap: &str) -> Result<(), JsError> {
      let script = patcher::frida::script(&self.scripts, bootstrap);
      self.frida = Some(Frida::new(gadget, script)?);
      Ok(())
   }

   /// Patches and merges the splits into one signed APK. `log` gets each
   /// step. The returned views stay valid until the next call into wasm.
   #[expect(
      clippy::needless_pass_by_value,
      reason = "wasm-bindgen hands strings over owned"
   )]
   pub fn merge(
      &mut self,
      package: Option<String>,
      label: Option<String>,
      log: &Function,
   ) -> Result<Array, JsError> {
      let package = match package {
         Some(name) => name.parse::<PackageName>()?,
         None => PackageName::default(),
      };
      let runtime = (!self.libraries.is_empty()).then(|| {
         Runtime {
            libraries: std::mem::take(&mut self.libraries),
            plugins:   std::mem::take(&mut self.plugins),
         }
      });
      let payload = Payload {
         runtime,
         frida: self.frida.take(),
      };
      let prepared = self.prepared.insert(payload.prepare()?);
      let mut log = |line: &str| {
         let _ = log.call1(&JsValue::NULL, &JsValue::from_str(line));
      };
      let apk = patcher::merge(
         &mut self.apks,
         prepared,
         &package,
         label.as_deref(),
         &mut log,
      )?;
      Ok(apk
         .into_parts()
         .into_iter()
         .map(|part| {
            match part {
               // SAFETY: the bytes belong to `self`, and the caller copies the
               // view before calling into wasm again, which could grow memory.
               Cow::Borrowed(bytes) => unsafe { Uint8Array::view(bytes) },
               Cow::Owned(bytes) => Uint8Array::from(&bytes[..]),
            }
         })
         .collect())
   }
}
