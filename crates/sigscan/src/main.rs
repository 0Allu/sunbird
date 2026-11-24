//! `sigscan <library.so | Discord.exe> [command]`
//!
//! Loads an ELF or PE64 binary the way the OS loader maps it and dumps the
//! gateway registry the runtime would hook. On x86-64 files it also finds
//! code for you:
//!
//! - `strings <text>`: where `text` sits, the code that loads it, and the
//!   function around each load.
//! - `refs <addr>`: the code and pointers that reference `addr`.
//! - `check`: runs every locator the runtime uses on this file's platform and
//!   exits 1 if any misses, so a new Discord build can be vetted before
//!   shipping against it.
//! - `dis <addr> [count]`: the bounds of the function holding `addr`, and
//!   `count` instructions from `addr` to that function's end.
//!
//! Addresses are hex at the file's preferred base. One below the base is an
//! RVA. Every address printed carries its RVA too, for a live process's base.

use std::{
   collections::HashMap,
   env,
   fs,
};

use misstep::{
   OptionExt,
   ResultExt,
};
use phoenix_hook::{
   image::Image,
   x86,
};
use phoenix_sigs::{
   Found,
   desktop,
   file::{
      BinaryFile,
      Machine,
   },
   http::DesktopHttpTransport,
   ios,
   mobile,
   registry::GatewayRegistry,
};

const USAGE: &str = "usage: sigscan <library.so | Discord.exe> [check | strings <text> | refs \
                     <addr> | dis <addr> [count]]";

fn main() -> misstep::Result<()> {
   misstep::install_panic_hook();
   let args = env::args().skip(1).collect::<Vec<String>>();
   let path = args.first().context(USAGE)?;
   let data = fs::read(path).with_context(|| format!("reading {path}"))?;
   let parsed = BinaryFile::parse(path, &data)?;
   let image = parsed.image();
   let base = image.pdata.map_or(0, |(base, ..)| base);
   let addr = |at: usize| -> misstep::Result<usize> {
      let text = args.get(at).context(USAGE)?;
      let value = usize::from_str_radix(text.trim_start_matches("0x"), 16)
         .with_context(|| format!("{text} is not a hex address"))?;
      Ok(if value < base { base + value } else { value })
   };
   let tools = Tools { image, base };
   match args.get(1).map(String::as_str) {
      None => dump(image),
      Some("check") => {
         check(
            image,
            parsed.machine(),
            matches!(parsed, BinaryFile::MachO(_)),
         )
      },
      Some("strings") => tools.strings(args.get(2).context(USAGE)?),
      Some("refs") => {
         tools.refs(addr(2)?);
         Ok(())
      },
      Some("dis") => {
         let count = args
            .get(3)
            .map_or(Ok(40), |count| count.parse::<usize>())
            .context(USAGE)?;
         tools.dis(addr(2)?, count);
         Ok(())
      },
      Some(_) => Err(misstep::Report::msg(USAGE)),
   }
}

/// Runs every locator for `machine` and prints what each found.
#[expect(clippy::too_many_lines, reason = "one report per locator")]
fn check(image: &Image<'_>, machine: Machine, macho: bool) -> misstep::Result<()> {
   let mut missed = 0;
   let mut report = |name: &str, found: Found<String>| {
      match found {
         Ok(detail) => println!("ok    {name:<20} {detail}"),
         Err(why) => {
            missed += 1;
            println!("MISS  {name:<20} {why}");
         },
      }
   };
   let at = |addr: usize| format!("{addr:#x}");
   let registry = GatewayRegistry::locate(image).map_err(|err| err.to_string());
   report(
      "gateway registry",
      registry
         .as_ref()
         .map(|registry| format!("{} events", registry.iter().count()))
         .map_err(Clone::clone),
   );
   match machine {
      Machine::X86_64 => {
         report(
            "http transport",
            DesktopHttpTransport::locate(image)
               .ok_or_else(|| "not found".to_owned())
               .and_then(|http| {
                  let head = image
                     .read(http.send_fn, desktop::FRAME_PROLOGUE_LEN)
                     .unwrap_or_default();
                  (desktop::FRAME_PROLOGUE.find_in(head).first() == Some(&0))
                     .then(|| at(http.send_fn))
                     .ok_or_else(|| "send prologue changed".to_owned())
               }),
         );
         report("access gate", desktop::access_gate(image).map(at));
         report(
            "staff flag",
            desktop::staff(image).map(|field| format!("{field:x?}")),
         );
         report("settings label", desktop::settings_label(image).map(at));
         report(
            "settings visibility",
            desktop::settings_visibility(image).map(at),
         );
         report(
            "settings section",
            desktop::settings_section(image)
               .map(|section| format!("{} (page +{:#x})", at(section.func), section.page_offset)),
         );
         let gpui = desktop::gpui(image);
         report(
            "gpui builders",
            gpui
               .as_ref()
               .map(|gpui| at(gpui.div_new))
               .map_err(Clone::clone),
         );
         report(
            "gpui layouts",
            gpui
               .as_ref()
               .map_err(|_| "needs the builders".to_owned())
               .and_then(|gpui| desktop::gpui_layouts(image, gpui))
               .map(|()| "match".to_owned()),
         );
         let row = desktop::message_row(image);
         report(
            "message row",
            row.as_ref()
               .map(|row| format!("{} (id +{:#x})", at(row.func), row.msg_id_offset))
               .map_err(Clone::clone),
         );
         report(
            "message body",
            row.as_ref()
               .map_err(|_| "needs the message row".to_owned())
               .and_then(|row| desktop::message_body(image, row.func))
               .map(|body| {
                  format!(
                     "{} (id +{:#x}, colors +{:#x})",
                     at(body.func),
                     body.msg_id_offset,
                     body.row_colors_offset
                  )
               }),
         );
         report(
            "optimistic delete",
            registry
               .as_ref()
               .map_err(|_| "needs the registry".to_owned())
               .and_then(|registry| {
                  let id = registry.id("MESSAGE_DELETE").ok_or("no MESSAGE_DELETE")?;
                  desktop::optimistic_delete(image, registry.entry(id).handler)
               })
               .map(at),
         );
      },
      Machine::Aarch64 if macho => report("guest bridge", ios::bridge(image).map(at)),
      Machine::Aarch64 => {
         report(
            "staff flag",
            mobile::staff_aarch64(image).map(|field| format!("{field:x?}")),
         );
         report("emoji check", mobile::emoji_aarch64(image).map(at));
      },
      Machine::Arm => {
         report("staff check", mobile::staff_arm(image).map(at));
         report("emoji check", mobile::emoji_arm(image).map(at));
      },
   }
   if missed > 0 {
      return Err(misstep::Report::msg(format!(
         "{missed} locators missed this build"
      )));
   }
   Ok(())
}

fn dump(image: &Image<'_>) -> misstep::Result<()> {
   match image.build_id {
      Some(id) => println!("{} build {id}", image.file_name()),
      None => println!("{} without a build ID", image.file_name()),
   }

   let registry = GatewayRegistry::locate(image)?;
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
   if let Some(http) = DesktopHttpTransport::locate(image) {
      let fmt_opt = |addr: Option<usize>| {
         addr.map_or_else(|| "none".to_owned(), |addr| format!("{addr:#010x}"))
      };
      println!(
         "http:     send={:#010x} ({} slots), poll={:#010x} ({} slots), alloc={}, dealloc={}",
         http.send_fn,
         http.send_slots.len(),
         http.poll_fn,
         http.poll_slots.len(),
         fmt_opt(http.host_alloc),
         fmt_opt(http.host_dealloc),
      );
   }
   Ok(())
}

struct Tools<'a> {
   image: &'a Image<'a>,
   base:  usize,
}

impl Tools<'_> {
   fn at(&self, addr: usize) -> String {
      format!("{addr:#x} (rva {:#x})", addr - self.base)
   }

   fn strings(&self, text: &str) -> misstep::Result<()> {
      let found = self.image.find_bytes(text.as_bytes());
      if found.is_empty() {
         return Err(misstep::Report::msg(format!("{text:?} is not in the file")));
      }
      for at in found.into_iter().take(16) {
         println!("{text:?} at {}", self.at(at));
         let loads = x86::literal_loads(self.image, at);
         match loads.first().and_then(|load| load.line.target) {
            None => println!("   no code loads it"),
            Some(start) if start != at => {
               println!("   literal starts {} bytes earlier", at - start);
            },
            Some(_) => {},
         }
         for load in loads.iter().take(32) {
            self.print_site(load);
         }
      }
      Ok(())
   }

   fn refs(&self, target: usize) {
      let code = x86::code_refs(self.image, target..target + 1);
      let pointers = self.image.pointers_to(target);
      println!(
         "{}: {} code refs, {} pointers",
         self.at(target),
         code.len(),
         pointers.len()
      );
      for site in code.iter().take(64) {
         self.print_site(site);
      }
      for pointer in pointers.into_iter().take(64) {
         println!("   pointer at {}", self.at(pointer));
      }
   }

   fn print_site(&self, site: &x86::CodeRef) {
      println!(
         "   {}  {}   in fn {} +{:#x}",
         self.at(site.line.addr),
         site.line.text,
         self.at(site.function),
         site.line.addr - site.function
      );
   }

   fn dis(&self, addr: usize, count: usize) {
      let mapped = self
         .image
         .segments
         .iter()
         .find(|segment| segment.contains(addr))
         .map_or(0, |segment| segment.addr + segment.bytes.len() - addr);
      let Some(code) = self
         .image
         .read(addr, mapped.min(16 * count))
         .filter(|code| !code.is_empty())
      else {
         println!("{} is not mapped", self.at(addr));
         return;
      };
      let end = match self.image.function_bounds(addr) {
         Some((start, end)) => {
            println!("fn {} .. {:#x}, {} bytes", self.at(start), end, end - start);
            end
         },
         None => usize::MAX,
      };
      for line in x86::disasm(code, addr, count) {
         if line.addr >= end {
            println!("   -- end of function --");
            break;
         }
         println!("   {}  {}", self.at(line.addr), line.text);
      }
   }
}
