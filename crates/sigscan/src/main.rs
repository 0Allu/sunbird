//! `sigscan <library.so>`
//!
//! Loads an ELF file the way the dynamic linker would see it, with relative
//! relocations applied. It walks the gateway registry, checks the build table's
//! rules when the build ID is known, and ends with a `Build` entry for
//! `builds.rs`. Code patterns go into that entry by hand from the printed
//! prologues, wildcarding PC-relative operands.

use std::{
   collections::HashMap,
   env,
   fs,
};

use misstep::{
   OptionExt,
   ResultExt,
   bail,
};
use plumage_sigs::{
   Build,
   GatewayRegistry,
   Image,
   Sig,
   file::ElfFile,
};

fn main() -> misstep::Result<()> {
   misstep::install_panic_hook();
   let path = env::args()
      .nth(1)
      .context("usage: sigscan <library.so>")?;
   let data = fs::read(&path).with_context(|| format!("reading {path}"))?;
   let image = ElfFile::parse(&path, &data)?.image;
   let build_id = image.build_id.context("no NT_GNU_BUILD_ID note")?;
   let known = Build::find(build_id);
   println!("{} build {build_id}", image.file_name());
   println!(
      "table: {}",
      known.map_or("not in BUILDS", |build| build.label)
   );

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

   let mut failed = false;
   if let Some(build) = known {
      for rule in build.gateway {
         failed |= report("table", &rule.sig, &image);
      }
   }
   if let Some((_, seed)) = registry
      .iter()
      .find(|(_, entry)| entry.name == "MESSAGE_CREATE")
   {
      failed |= report("portable", &Sig::gateway(&seed.name), &image);
   }

   println!("\n   Build {{");
   println!("      id: BuildId::hex(\"{build_id}\"),");
   println!("      label: \"TODO\",");
   println!("      library: \"{}\",", image.file_name());
   println!("      gateway: &[],");
   println!("   }},");
   if failed {
      bail!("some rules did not resolve");
   }
   Ok(())
}

fn report<A: plumage_sigs::Abi>(kind: &str, sig: &Sig<'_, A>, image: &Image<'_>) -> bool {
   match sig.resolve(image) {
      Ok(located) => {
         let prologue = image
            .segments
            .iter()
            .find(|segment| segment.contains(located.addr))
            .and_then(|segment| segment.bytes.get(located.addr - segment.addr..)?.get(..24))
            .map_or_default(|bytes| {
               bytes
                  .iter()
                  .map(|byte| format!("{byte:02X}"))
                  .collect::<Vec<_>>()
                  .join(" ")
            });
         println!(
            "{kind:>8} {:<20} {:#010x} {}/{} anchors, first `{}`\n{:>8} {:<20} {prologue}",
            sig.name,
            located.addr,
            located.agreeing,
            sig.anchors.len(),
            sig.anchors[located.anchor],
            "",
            ""
         );
         false
      },
      Err(err) => {
         println!("{kind:>8} {err}");
         true
      },
   }
}
