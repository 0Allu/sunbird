//! The Discord update page, drawn as cards from plain views: what the update
//! brings and what Sunbird checked, then a progress bar over each step, and
//! the failure or the hand-off to Android's installer.

use phoenix_ui_mobile::{
   Prop,
   Props,
   ViewKind,
   prop,
   templates,
};
use phoenix_wire::Value;

use crate::{
   discord_update::{
      self,
      Build,
      Progress,
      State,
   },
   settings::{
      ICON_RELOAD,
      Press,
      theme::{
         Color,
         template_color,
      },
      tree::{
         Tree,
         text,
      },
   },
};

const BLURPLE: i64 = 0xFF58_65F2;
const GREEN: i64 = 0xFF23_A55A;
const WHITE: i64 = 0xFFFF_FFFF;
/// The settings rows' translucent surface and the track under a bar, as the
/// captured dark layouts have them; [`template_color`] maps them for light.
const SURFACE: i64 = 0x1497_979F;
const TRACK: i64 = 0x1F97_979F;

pub fn sections() -> Vec<Tree> {
   let card = match discord_update::state() {
      State::Current => {
         card(vec![line(
            "Discord is on the newest build Sunbird has checked.",
            Color::Muted.argb(),
         )])
      },
      State::Available(build) => offer(&build),
      State::Running(build, progress) => running(&build, &progress),
      State::Failed { build, error } => {
         card(vec![
            heading(
               Color::Error.argb(),
               "!",
               "Update failed",
               &build.version_name,
            ),
            line(&error, Color::Muted.argb()),
            button("Try again", BLURPLE),
         ])
      },
      State::Handed(build) => {
         card(vec![
            heading(GREEN, "\u{2713}", "Ready to install", &build.version_name),
            line(
               "Android's installer has the update. Tap Update there to finish, and Discord \
                restarts on the new version.",
               Color::Muted.argb(),
            ),
            button("Open the installer", BLURPLE),
         ])
      },
   };
   vec![card]
}

fn offer(build: &Build) -> Tree {
   let from = discord_update::installed_name();
   let subtitle = if from.is_empty() {
      build.version_name.clone()
   } else {
      format!("{from}  \u{2192}  {}", build.version_name)
   };
   let size = discord_update::download_size(build) >> 20;
   card(vec![
      heading(BLURPLE, "", "Discord update", &subtitle),
      column(6.0, vec![
         check("Signed by Discord"),
         check("Sunbird's patches and plugin hooks fit this build"),
         check("Every download is checked before it installs"),
         check("You stay logged in"),
      ]),
      button(&format!("Download and install \u{00B7} {size} MB"), BLURPLE),
   ])
}

fn running(build: &Build, progress: &Progress) -> Tree {
   #[expect(
      clippy::cast_precision_loss,
      reason = "a bar's width needs no byte precision"
   )]
   let fraction = if progress.total == 0 {
      0.0
   } else {
      (progress.done as f64 / progress.total as f64).clamp(0.0, 1.0)
   };
   #[expect(
      clippy::cast_possible_truncation,
      clippy::cast_sign_loss,
      reason = "a percentage between 0 and 100"
   )]
   let percent = (fraction * 100.0).round() as u32;
   let steps = progress
      .steps
      .iter()
      .enumerate()
      .map(|(at, step)| {
         let (glyph, glyph_color, label_color, weight) = match at.cmp(&progress.at) {
            std::cmp::Ordering::Less => ("\u{2713}", GREEN, Color::Muted.argb(), 400),
            std::cmp::Ordering::Equal => ("\u{25CF}", BLURPLE, Color::Text.argb(), 600),
            std::cmp::Ordering::Greater => {
               ("\u{25CB}", Color::Muted.argb(), Color::Muted.argb(), 400)
            },
         };
         row(10.0, vec![
            fixed(label_node(glyph, 15.0, 700, glyph_color), 18.0),
            grow(label_node(step, 15.0, weight, label_color)),
         ])
      })
      .collect();
   card(vec![
      row(8.0, vec![
         grow(label_node(
            &format!("Updating to {}", build.version_name),
            17.0,
            700,
            Color::Text.argb(),
         )),
         label_node(&format!("{percent}%"), 17.0, 700, BLURPLE),
      ]),
      bar(fraction),
      line(
         &format!("{} of {} MB", progress.done >> 20, progress.total >> 20),
         Color::Muted.argb(),
      ),
      column(10.0, steps),
      line(
         "Keep Discord open until Android's installer shows.",
         Color::Muted.argb(),
      ),
   ])
}

/// A badge holding `glyph` (or the update icon), a title and a subtitle.
fn heading(color: i64, glyph: &str, title: &str, subtitle: &str) -> Tree {
   let inner = if glyph.is_empty() {
      node(
         ViewKind::Image,
         vec![
            (Prop::URI, text(ICON_RELOAD)),
            (prop!("width"), Value::F64(26.0)),
            (Prop::HEIGHT, Value::F64(26.0)),
            (Prop::TINT, Value::I64(WHITE)),
            (prop!("scale"), Value::Str("fit".to_owned())),
         ],
         Vec::new(),
      )
   } else {
      label_node(glyph, 22.0, 700, WHITE)
   };
   let badge = node(
      ViewKind::View,
      vec![
         (prop!("width"), Value::F64(48.0)),
         (Prop::HEIGHT, Value::F64(48.0)),
         (Prop::RADIUS, Value::F64(24.0)),
         (prop!("background"), Value::I64(color)),
         (prop!("align"), Value::Str("center".to_owned())),
         (prop!("justify"), Value::Str("center".to_owned())),
      ],
      vec![inner],
   );
   row(14.0, vec![
      badge,
      grow(column(2.0, vec![
         label_node(title, 18.0, 700, Color::Text.argb()),
         label_node(subtitle, 14.0, 400, Color::Muted.argb()),
      ])),
   ])
}

fn check(label: &str) -> Tree {
   row(8.0, vec![
      fixed(label_node("\u{2713}", 14.0, 700, GREEN), 16.0),
      grow(label_node(label, 14.0, 400, Color::Muted.argb())),
   ])
}

/// A full-width button that starts the update, or retries or reopens it.
fn button(label: &str, color: i64) -> Tree {
   let on = templates()
      .button_row
      .props
      .iter()
      .find(|(key, _)| *key == Prop::ON)
      .map_or_else(
         || Value::Arr(vec![text("press")]),
         |(_, value)| value.clone(),
      );
   let mut button = node(
      ViewKind::View,
      vec![
         (prop!("width"), Value::Str("fill".to_owned())),
         (Prop::HEIGHT, Value::F64(48.0)),
         (Prop::RADIUS, Value::F64(10.0)),
         (prop!("background"), Value::I64(color)),
         (prop!("activeBackground"), Value::I64(color & 0xCCFF_FFFF)),
         (prop!("align"), Value::Str("center".to_owned())),
         (prop!("justify"), Value::Str("center".to_owned())),
         (Prop::ROLE, text("button")),
         (Prop::ACCESSIBILITY_LABEL, text(label)),
         (Prop::ACCESSIBILITY_ID, text(&format!("phoenix-{label}"))),
         (Prop::ON, on),
      ],
      vec![label_node(label, 16.0, 700, WHITE)],
   );
   button.press = Some(Press::StartDiscordUpdate);
   button
}

/// A bar filled `fraction` of the way, as two flex weights.
fn bar(fraction: f64) -> Tree {
   let part = |weight: f64, color: i64| {
      node(
         ViewKind::View,
         vec![
            (prop!("flex"), Value::F64(weight)),
            (Prop::HEIGHT, Value::F64(8.0)),
            (prop!("background"), Value::I64(color)),
            (Prop::RADIUS, Value::F64(4.0)),
         ],
         Vec::new(),
      )
   };
   node(
      ViewKind::View,
      vec![
         (prop!("direction"), Value::Str("row".to_owned())),
         (prop!("width"), Value::Str("fill".to_owned())),
         (Prop::HEIGHT, Value::F64(8.0)),
         (Prop::RADIUS, Value::F64(4.0)),
         (prop!("clip"), Value::Bool(true)),
         (prop!("background"), Value::I64(template_color(TRACK))),
      ],
      vec![part(fraction, BLURPLE), part(1.0 - fraction, 0)],
   )
}

fn card(kids: Vec<Tree>) -> Tree {
   node(
      ViewKind::View,
      vec![
         (prop!("direction"), Value::Str("column".to_owned())),
         (prop!("width"), Value::Str("fill".to_owned())),
         (prop!("align"), Value::Str("stretch".to_owned())),
         (prop!("background"), Value::I64(template_color(SURFACE))),
         (Prop::RADIUS, Value::F64(16.0)),
         (Prop::PADDING, Value::Rect([18.0, 18.0, 18.0, 18.0])),
         (Prop::GAP, Value::F64(16.0)),
      ],
      kids,
   )
}

fn row(gap: f64, kids: Vec<Tree>) -> Tree {
   node(
      ViewKind::View,
      vec![
         (prop!("direction"), Value::Str("row".to_owned())),
         (prop!("width"), Value::Str("fill".to_owned())),
         (prop!("align"), Value::Str("center".to_owned())),
         (Prop::GAP, Value::F64(gap)),
      ],
      kids,
   )
}

fn column(gap: f64, kids: Vec<Tree>) -> Tree {
   node(
      ViewKind::View,
      vec![
         (prop!("direction"), Value::Str("column".to_owned())),
         (prop!("width"), Value::Str("fill".to_owned())),
         (prop!("align"), Value::Str("stretch".to_owned())),
         (Prop::GAP, Value::F64(gap)),
      ],
      kids,
   )
}

/// A paragraph across the card.
fn line(value: &str, color: i64) -> Tree {
   let mut node = label_node(value, 14.0, 400, color);
   node.set(prop!("width"), Some(Value::Str("fill".to_owned())));
   node
}

fn label_node(value: &str, size: f64, weight: i32, color: i64) -> Tree {
   node(
      ViewKind::Text,
      vec![
         (Prop::TEXT, text(value)),
         (Prop::COLOR, Value::I64(color)),
         (Prop::FONT_SIZE, Value::F64(size)),
         (prop!("fontWeight"), Value::I32(weight)),
         (Prop::LINE_HEIGHT, Value::F64(size + 6.0)),
      ],
      Vec::new(),
   )
}

fn grow(mut tree: Tree) -> Tree {
   tree.set(prop!("flex"), Some(Value::F64(1.0)));
   tree
}

fn fixed(mut tree: Tree, width: f64) -> Tree {
   tree.set(prop!("width"), Some(Value::F64(width)));
   tree
}

const fn node(kind: ViewKind, props: Props, kids: Vec<Tree>) -> Tree {
   Tree {
      id: 0,
      kind,
      props,
      kids,
      src: None,
      press: None,
      slot: None,
      animate: None,
   }
}
