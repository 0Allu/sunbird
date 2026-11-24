//! `sigscan <library.so>`
//!
//! Loads an ELF file the way the dynamic linker would see it, with relative
//! relocations applied, and dumps the gateway registry the runtime would hook.

use std::{
   collections::HashMap,
   env,
   fs,
};

use misstep::{
   OptionExt,
   ResultExt,
};
use phoenix_sigs::{
   file::ElfFile,
   registry::GatewayRegistry,
};

fn main() -> misstep::Result<()> {
   misstep::install_panic_hook();
   let path = env::args().nth(1).context("usage: sigscan <library.so>")?;
   let data = fs::read(&path).with_context(|| format!("reading {path}"))?;
   let image = ElfFile::parse(&path, &data)?.image;
   match image.build_id {
      Some(id) => println!("{} build {id}", image.file_name()),
      None => println!("{} without a build ID", image.file_name()),
   }

   let registry = GatewayRegistry::locate(&image)?;
   let mut sharing: HashMap<usize, Vec<&str>> = HashMap::new();
   for (_, entry) in registry.iter() {
      sharing.entry(entry.handler).or_default().push(&entry.name);
   }
   println!(
      "registry: {} events, {} handlers",
      registry.iter().count(),
      sharing.len()
   );

   for (_, entry) in registry.iter() {
      let shared = sharing[&entry.handler]
         .iter()
         .filter(|name| **name != entry.name)
         .collect::<Vec<&&str>>();

      let note = if shared.is_empty() {
         String::new()
      } else {
         format!(" shared with {shared:?}")
      };

      println!("   {:<40} {:#010x}{note}", entry.name, entry.handler);
   }
   Ok(())
}
