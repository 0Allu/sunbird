//! The runtime calls behind presses, submits and picked files.

use std::sync::atomic::Ordering;

use crate::{
   Chosen,
   Control,
   PluginInfo,
   settings::{
      COLOR_ERROR,
      COLOR_STRING,
      Field,
      Job,
      LOG_QUEUED,
      Press,
      TITLE,
      settings,
   },
};

fn plugin(index: usize) -> Option<PluginInfo> {
   settings().plugins.get(index).cloned()
}

/// Makes the runtime calls `job` asks for. They take the engine lock, so they
/// run before the settings lock is taken.
pub fn act(job: &Job) {
   match *job {
      Job::Log => LOG_QUEUED.store(false, Ordering::Relaxed),
      // The editor is multiline: Enter is a newline there, and Save saves.
      Job::Submit(Field::Code(_)) | Job::NavigateTo(_) | Job::Draft(..) | Job::Refresh => {},
      Job::Submit(field) => submit(field),
      Job::Picked(ref picked) => {
         match picked {
            Ok(Some((file, bytes))) => installed(crate::install_file(file, bytes.clone()), file),
            Ok(None) => {},
            Err(err) => {
               log::error!("file picker: {err}");
               settings().install_status =
                  Some((format!("The file picker failed: {err}"), COLOR_ERROR));
            },
         }
      },
      Job::Press(pressed) => press(pressed),
   }
}

fn submit(field: Field) {
   let Some(draft) = settings().drafts.remove(&field) else {
      return;
   };
   let input = draft.trim();
   match field {
      Field::Eval if !input.is_empty() => crate::evaluate(input),
      Field::Install if !input.is_empty() => {
         installed(crate::install(input), input);
      },
      Field::Setting(index, at) => {
         let Some(plugin) = plugin(index) else {
            return;
         };
         let Some(setting) = plugin.settings.get(at) else {
            return;
         };
         let value = match setting.control {
            Control::Text(_) | Control::Secret(_) => Chosen::Text(draft.clone()),
            Control::Number(_) => {
               let Ok(number) = input.parse::<f64>() else {
                  log::error!(
                     "[{}] {}: {input:?} is not a number",
                     plugin.name,
                     setting.key
                  );
                  return;
               };
               Chosen::Number(number)
            },
            Control::Switch(_) | Control::Select { .. } | Control::Button => return,
         };
         crate::set_setting(&plugin.name, &setting.key, value);
      },
      Field::Eval | Field::Install | Field::Code(_) => {},
   }
}

fn press(pressed: Press) {
   match pressed {
      Press::PickFile => {
         if let Err(err) = crate::picker::pick() {
            log::error!("file picker: {err}");
            settings().install_status =
               Some((format!("The file picker failed: {err}"), COLOR_ERROR));
         }
      },
      // The editor starts from the plugin's code, kept as the field's draft.
      Press::EditCode(index) => {
         if let Some(plugin) = plugin(index)
            && let Some(source) = crate::source(&plugin.name)
         {
            settings().drafts.insert(Field::Code(index), source);
         }
      },
      Press::SaveCode(index) => {
         let draft = settings().drafts.get(&Field::Code(index)).cloned();
         if let (Some(plugin), Some(source)) = (plugin(index), draft) {
            match crate::save_source(&plugin.name, source) {
               Ok(()) => log::info!("saved {} and restarted it", plugin.name),
               Err(err) => log::error!("saving {} failed: {err}", plugin.name),
            }
         }
      },
      Press::Toggle(index) => {
         if let Some(plugin) = plugin(index) {
            crate::set_enabled(&plugin.name, !plugin.enabled);
         }
      },
      Press::Reload(index) => {
         if let Some(plugin) = plugin(index) {
            crate::reload(&plugin.name);
         }
      },
      Press::Uninstall(index) => {
         if let Some(plugin) = plugin(index) {
            match crate::uninstall(&plugin.name) {
               Ok(()) => log::info!("uninstalled {}", plugin.name),
               Err(err) => log::error!("uninstall failed: {err}"),
            }
         }
      },
      Press::Setting(index, at) => {
         let Some(plugin) = plugin(index) else {
            return;
         };
         match plugin
            .settings
            .get(at)
            .map(|setting| (&setting.key, &setting.control))
         {
            Some((key, Control::Switch(on))) => {
               crate::set_setting(&plugin.name, key, Chosen::Switch(!on));
            },
            Some((key, Control::Button)) => crate::press(&plugin.name, key),
            _ => {},
         }
      },
      Press::Choose(index, at, option) => {
         let Some(plugin) = plugin(index) else {
            return;
         };
         if let Some(setting) = plugin.settings.get(at)
            && let Control::Select { options, .. } = &setting.control
            && let Some(choice) = options.get(option)
         {
            crate::set_setting(
               &plugin.name,
               &setting.key,
               Chosen::Text(choice.value.clone()),
            );
         }
      },
      Press::ApplyUpdate => {
         crate::update::apply();
      },
      Press::OpenSite => {
         if let Err(err) = crate::picker::open(crate::update::SITE) {
            log::error!("opening {}: {err}", crate::update::SITE);
         }
      },
      Press::ToggleStaff => {
         crate::set_staff_enabled(!crate::staff_enabled());
      },
      Press::ToggleRemoteShell => {
         crate::set_remote_shell_enabled(!crate::remote_shell_enabled());
      },
      Press::OpenMain
      | Press::OpenConsole
      | Press::OpenInstall
      | Press::OpenPlugin(_)
      | Press::Back => {},
   }
}

/// Logs an install's outcome and keeps it for the install page.
fn installed(result: Result<String, crate::Error>, from: &str) {
   let status = match result {
      Ok(name) => {
         log::info!("installed {name} from {from}");
         (
            format!("Installed {name}. It is on the {TITLE} page now."),
            COLOR_STRING,
         )
      },
      Err(err) => {
         log::error!("install failed: {err}");
         (format!("Install failed: {err}"), COLOR_ERROR)
      },
   };
   settings().install_status = Some(status);
}
