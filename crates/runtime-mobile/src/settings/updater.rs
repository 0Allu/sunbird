//! The Discord update page: what the update brings and what Sunbird checked,
//! then a progress bar over each step with its size, and the failure or the
//! hand-off to Android's installer.

use phoenix_ui_mobile::{
   Prop,
   ViewKind,
   prop,
};
use phoenix_wire::Value;

use crate::{
   discord_update::{
      self,
      Build,
      Progress,
      Stage,
      State,
   },
   settings::{
      ICON_RELOAD,
      Press,
      cards::{
         BLURPLE,
         GREEN,
         badge,
         bar,
         button,
         card,
         column,
         fixed,
         grow,
         heading,
         label,
         line,
         node,
         row,
      },
      theme::{
         Color,
         template_color,
      },
      tree::Tree,
   },
};

pub fn sections() -> Vec<Tree> {
   let installed = discord_update::installed_name();
   let card = match discord_update::state() {
      State::Current => {
         card(vec![heading(
            badge(GREEN, 52.0, "\u{2713}"),
            "Discord is up to date",
            &installed,
            None,
         )])
      },
      State::Available(build) => offer(&build, &installed),
      State::Running(build, progress) => running(&build, &progress, &installed),
      State::Failed { build, error } => {
         card(vec![
            heading(
               badge(Color::Error.argb(), 52.0, "!"),
               "Update failed",
               &format!("Discord {}", build.version_name),
               None,
            ),
            line(&error, Color::Muted.argb()),
            button("Try again", Press::StartDiscordUpdate),
         ])
      },
      State::Handed(build) => {
         card(vec![
            heading(
               badge(GREEN, 52.0, "\u{2713}"),
               "Ready to install",
               &format!("Discord {}", build.version_name),
               None,
            ),
            line(
               "Android's installer has the update. Tap Update there, and Discord restarts on the \
                new version.",
               Color::Muted.argb(),
            ),
            button("Open the installer", Press::StartDiscordUpdate),
         ])
      },
   };
   vec![card]
}

fn offer(build: &Build, installed: &str) -> Tree {
   let size = discord_update::download_size(build) >> 20;
   let subtitle = if installed.is_empty() {
      "A newer Discord is ready".to_owned()
   } else {
      format!("You have {installed}")
   };
   card(vec![
      heading(
         badge(BLURPLE, 52.0, ICON_RELOAD),
         &format!("Discord {}", build.version_name),
         &subtitle,
         None,
      ),
      row(8.0, vec![
         chip(&format!("{size} MB")),
         chip("Signed by Discord"),
         chip("Login kept"),
      ]),
      column(10.0, vec![
         label("What Sunbird checked", 13.0, 700, Color::Muted.argb()),
         check("Discord's own signature on every file"),
         check("Sunbird's patches and plugin hooks fit this build"),
         check("Each download matches before it installs"),
      ]),
      button(
         &format!("Download and install \u{00B7} {size} MB"),
         Press::StartDiscordUpdate,
      ),
   ])
}

fn running(build: &Build, progress: &Progress, installed: &str) -> Tree {
   let step = progress.steps.get(progress.at);
   let stage = step.map_or(Stage::Download, |step| step.stage);
   let action = step.map_or("Getting ready", |step| step.action.as_str());
   let versions = if installed.is_empty() {
      build.version_name.clone()
   } else {
      format!("{installed}  \u{2192}  {}", build.version_name)
   };
   let pace = progress.pace().map_or_else(String::new, |(speed, left)| {
      let left = if left < 60 {
         format!("{left} s left")
      } else {
         format!("{} min left", left.div_ceil(60))
      };
      format!("{:.1} MB/s \u{00B7} {left}", speed / f64::from(1 << 20))
   });
   centered(vec![
      badge(BLURPLE, 64.0, ICON_RELOAD),
      column(4.0, vec![
         center(label("Updating Discord", 20.0, 700, Color::Text.argb())),
         center(label(&versions, 14.0, 400, Color::Muted.argb())),
      ]),
      center(label(
         &format!("{}%", progress.percent()),
         44.0,
         700,
         BLURPLE,
      )),
      column(8.0, vec![
         bar(progress.fraction(), 12.0),
         row(8.0, vec![
            grow(label(
               &format!(
                  "{} of {}",
                  megabytes(progress.done),
                  megabytes(progress.total)
               ),
               13.0,
               400,
               Color::Muted.argb(),
            )),
            label(&pace, 13.0, 400, Color::Muted.argb()),
         ]),
      ]),
      center(label(action, 15.0, 600, Color::Text.argb())),
      row(8.0, vec![
         stage_chip("Download", Stage::Download, stage),
         stage_chip("Build", Stage::Build, stage),
         stage_chip("Install", Stage::Install, stage),
      ]),
      center(label(
         "Keep Discord open, the installer opens when it is ready.",
         13.0,
         400,
         Color::Muted.argb(),
      )),
   ])
}

/// One of the update's three stages, marked done, under way or waiting.
fn stage_chip(name: &str, this: Stage, at: Stage) -> Tree {
   let (glyph, text_color, background) = match this.cmp(&at) {
      std::cmp::Ordering::Less => ("\u{2713} ", GREEN, GREEN & 0x33FF_FFFF),
      std::cmp::Ordering::Equal => ("", BLURPLE, BLURPLE & 0x33FF_FFFF),
      std::cmp::Ordering::Greater => ("", Color::Muted.argb(), template_color(0x1F97_979F)),
   };
   grow(node(
      ViewKind::View,
      vec![
         (Prop::HEIGHT, Value::F64(36.0)),
         (Prop::RADIUS, Value::F64(10.0)),
         (prop!("background"), Value::I64(background)),
         (prop!("align"), Value::Str("center".to_owned())),
         (prop!("justify"), Value::Str("center".to_owned())),
      ],
      vec![label(&format!("{glyph}{name}"), 14.0, 700, text_color)],
   ))
}

/// A card whose items sit centered.
fn centered(kids: Vec<Tree>) -> Tree {
   let mut card = card(kids);
   card.set(prop!("align"), Some(Value::Str("center".to_owned())));
   card.set(Prop::PADDING, Some(Value::Rect([20.0, 28.0, 20.0, 24.0])));
   card.set(Prop::GAP, Some(Value::F64(18.0)));
   card
}

/// A text centered across its parent.
fn center(mut tree: Tree) -> Tree {
   tree.set(prop!("width"), Some(Value::Str("fill".to_owned())));
   tree.set(prop!("textAlign"), Some(Value::Str("center".to_owned())));
   tree
}

/// `bytes` in MB, or KB under one.
fn megabytes(bytes: usize) -> String {
   if bytes < 1 << 20 {
      format!("{} KB", bytes.div_ceil(1 << 10))
   } else {
      format!("{} MB", bytes >> 20)
   }
}

fn check(text_value: &str) -> Tree {
   row(10.0, vec![
      fixed(label("\u{2713}", 14.0, 700, GREEN), 16.0),
      grow(label(text_value, 14.0, 400, Color::Text.argb())),
   ])
}

/// A small tinted tag.
fn chip(text_value: &str) -> Tree {
   node(
      ViewKind::View,
      vec![
         (Prop::PADDING, Value::Rect([10.0, 4.0, 10.0, 4.0])),
         (Prop::RADIUS, Value::F64(12.0)),
         (prop!("background"), Value::I64(template_color(0x1F97_979F))),
      ],
      vec![label(text_value, 12.0, 600, Color::Text.argb())],
   )
}
