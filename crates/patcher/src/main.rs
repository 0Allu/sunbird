//! `patcher --out <dir> --loader <libplumage.so> [--lib <extra.so>]...
//! [--plugin <name.js>]... <split.apk>...`
//!
//! Rewrites Discord's split APKs so Phoenix starts with plumage inside it. The
//! base split's launcher always picks Phoenix, the host library's access gate
//! returns true and loads the loader through a renamed `DT_NEEDED` entry, and
//! the loader, any extra libraries and the plugin bundle land next to the host
//! library in its ABI split. Every split is then zipaligned and signed with the debug key, so
//! `adb install-multiple <dir>/*.apk` installs the set.

mod dex;
mod host;

use std::{
   collections::BTreeMap,
   env,
   fs::{
      self,
      File,
   },
   io::{
      Read,
      Write,
   },
   path::{
      Path,
      PathBuf,
   },
   process::Command,
};

use misstep::{
   OptionExt,
   ResultExt,
   bail,
   ensure,
};
use plumage_sigs::{
   PLUGIN_BUNDLE,
   file::{
      ElfFile,
      Machine,
   },
};
use zip::{
   CompressionMethod,
   ZipArchive,
   ZipWriter,
   write::SimpleFileOptions,
};

struct Args {
   out:     PathBuf,
   loader:  PathBuf,
   extra:   Vec<PathBuf>,
   plugins: Vec<PathBuf>,
   apks:    Vec<PathBuf>,
}

impl Args {
   fn parse() -> misstep::Result<Self> {
      const USAGE: &str =
         "usage: patcher --out <dir> --loader <libplumage.so> [--lib <extra.so>]... [--plugin <name.js>]... <split.apk>...";
      let (mut out, mut loader) = (None, None);
      let (mut extra, mut plugins, mut apks) = (Vec::new(), Vec::new(), Vec::new());
      let mut args = env::args_os().skip(1);
      while let Some(arg) = args.next() {
         let mut value = || args.next().map(PathBuf::from).context(USAGE);
         match arg.to_str() {
            Some("--out") => out = Some(value()?),
            Some("--loader") => loader = Some(value()?),
            Some("--lib") => extra.push(value()?),
            Some("--plugin") => plugins.push(value()?),
            Some(flag) if flag.starts_with("--") => bail!("unknown flag {flag}\n{USAGE}"),
            _ => apks.push(PathBuf::from(arg)),
         }
      }
      ensure!(!apks.is_empty(), "{USAGE}");
      Ok(Self {
         out: out.context(USAGE)?,
         loader: loader.context(USAGE)?,
         extra,
         plugins,
         apks,
      })
   }
}

/// A library added to the ABI split, under its name in the APK.
struct Library {
   name:    String,
   machine: Machine,
   bytes:   Vec<u8>,
}

impl Library {
   fn read(path: &Path, name: Option<&str>) -> misstep::Result<Self> {
      let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
      let display = path.display().to_string();
      let machine = ElfFile::parse(&display, &bytes)?.machine;
      let name = match name {
         Some(name) => name.to_owned(),
         None => {
            path
               .file_name()
               .and_then(|name| name.to_str())
               .context("library path has no UTF-8 file name")?
               .to_owned()
         },
      };
      Ok(Self {
         name,
         machine,
         bytes,
      })
   }
}

#[derive(Default)]
struct Progress {
   dex:  bool,
   host: Vec<Machine>,
}

fn main() -> misstep::Result<()> {
   misstep::install_panic_hook();
   let args = Args::parse()?;
   let tools = BuildTools::find()?;
   fs::create_dir_all(&args.out)?;

   let mut libraries = vec![Library::read(&args.loader, Some(host::INJECTED))?];
   for path in &args.extra {
      libraries.push(Library::read(path, None)?);
   }
   let mut bundle = BTreeMap::new();
   for path in &args.plugins {
      let name = path
         .file_stem()
         .and_then(|stem| stem.to_str())
         .context("plugin path has no UTF-8 file name")?;
      let source = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
      bundle.insert(name.to_owned(), source);
   }
   let bundle = serde_json::to_vec(&bundle)?;

   let mut progress = Progress::default();
   for apk in &args.apks {
      let name = apk.file_name().context("APK path has no file name")?;
      let unsigned = args.out.join(name).with_extension("unsigned");
      rewrite(apk, &unsigned, &libraries, &bundle, &mut progress)
         .with_context(|| format!("rewriting {}", apk.display()))?;
      tools.align_and_sign(&unsigned, &args.out.join(name))?;
      println!("signed {}", name.display());
   }
   ensure!(
      progress.dex && !progress.host.is_empty(),
      "incomplete, launcher patched: {}, host patched for {:?}. Pass base.apk and the ABI split.",
      progress.dex,
      progress.host,
   );
   Ok(())
}

fn rewrite(
   source: &Path,
   destination: &Path,
   libraries: &[Library],
   bundle: &[u8],
   progress: &mut Progress,
) -> misstep::Result<()> {
   let mut archive = ZipArchive::new(File::open(source)?)?;
   let mut writer = ZipWriter::new(File::create(destination)?);
   let mut abi_dirs = Vec::new();

   for index in 0..archive.len() {
      let (name, method) = {
         let file = archive.by_index_raw(index)?;
         (file.name()?.into_owned(), file.compression())
      };
      if is_v1_signature(&name) {
         continue;
      }
      let is_dex = !progress.dex
         && name
            .strip_prefix("classes")
            .is_some_and(|rest| rest.strip_suffix(".dex").is_some());
      let host_dir = name
         .strip_suffix(host::LIBRARY)
         .filter(|dir| dir.starts_with("lib/"));
      if !is_dex && host_dir.is_none() {
         writer.raw_copy_file(archive.by_index_raw(index)?)?;
         continue;
      }

      let mut data = Vec::new();
      archive.by_index(index)?.read_to_end(&mut data)?;
      if let Some(dir) = host_dir {
         let patched = host::patch(&name, &mut data)?;
         println!(
            "{name}: access evaluator {:#x} returns true, liblog.so -> {}",
            patched.access,
            host::INJECTED
         );
         abi_dirs.push((dir.to_owned(), patched.machine));
         progress.host.push(patched.machine);
      } else if let Some(code) = dex::patch(&mut data)? {
         println!("{name}: launcher at {code:#x} returns true");
         progress.dex = true;
      }
      writer.start_file(
         &name,
         SimpleFileOptions::default().compression_method(method),
      )?;
      writer.write_all(&data)?;
   }

   // Stored, so the linker maps them straight out of the APK like the others.
   let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
   for (dir, machine) in &abi_dirs {
      for library in libraries
         .iter()
         .filter(|library| library.machine == *machine)
      {
         let name = format!("{dir}{}", library.name);
         ensure!(
            archive.index_for_name(&name).is_none(),
            "{name} is already in the APK"
         );
         writer.start_file(&name, stored)?;
         writer.write_all(&library.bytes)?;
         println!("added {name}");
      }
      let name = format!("{dir}{PLUGIN_BUNDLE}");
      writer.start_file(&name, stored)?;
      writer.write_all(bundle)?;
      println!("added {name}");
   }
   writer.finish()?;
   Ok(())
}

/// JAR signature files, which apksigner would otherwise keep beside a stale
/// manifest digest.
fn is_v1_signature(name: &str) -> bool {
   let Some(file) = name.strip_prefix("META-INF/") else {
      return false;
   };
   !file.contains('/')
      && (file == "MANIFEST.MF"
         || [".SF", ".RSA", ".DSA", ".EC"]
            .iter()
            .any(|suffix| file.ends_with(suffix)))
}

struct BuildTools {
   dir: PathBuf,
   key: PathBuf,
}

impl BuildTools {
   fn find() -> misstep::Result<Self> {
      let sdk = env::var_os("ANDROID_SDK_ROOT")
         .context("ANDROID_SDK_ROOT is unset, enter the devshell with `nix develop`")?;
      let mut versions = fs::read_dir(Path::new(&sdk).join("build-tools"))?
         .map(|entry| entry.map(|entry| entry.path()))
         .collect::<Result<Vec<PathBuf>, _>>()?;
      versions.sort();

      let home = env::var_os("HOME").context("HOME is unset")?;
      let key = Path::new(&home).join(".android/debug.keystore");
      ensure!(key.exists(), "{} is missing", key.display());

      Ok(Self {
         dir: versions.pop().context("no Android build-tools installed")?,
         key,
      })
   }

   fn align_and_sign(&self, unsigned: &Path, signed: &Path) -> misstep::Result<()> {
      let aligned = unsigned.with_extension("aligned");
      // -P 16 page-aligns stored libraries for 16 KiB page kernels.
      self.run(
         Command::new(self.dir.join("zipalign"))
            .args(["-P", "16", "-f", "4"])
            .arg(unsigned)
            .arg(&aligned),
      )?;
      self.run(
         Command::new(self.dir.join("apksigner"))
            .args(["sign", "--ks"])
            .arg(&self.key)
            .args([
               "--ks-key-alias",
               "androiddebugkey",
               "--ks-pass",
               "pass:android",
               "--key-pass",
               "pass:android",
               "--out",
            ])
            .arg(signed)
            .arg(&aligned),
      )?;
      fs::remove_file(unsigned)?;
      fs::remove_file(aligned)?;
      Ok(())
   }

   #[expect(clippy::unused_self, reason = "keeps the tool invocations together")]
   fn run(&self, command: &mut Command) -> misstep::Result<()> {
      let status = command
         .status()
         .with_context(|| format!("running {}", command.get_program().display()))?;
      ensure!(
         status.success(),
         "{} failed with {status}",
         command.get_program().display()
      );
      Ok(())
   }
}
