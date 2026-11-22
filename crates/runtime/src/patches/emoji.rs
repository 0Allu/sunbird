//! Custom emoji presses in the emoji picker under the message box, for
//! `phoenix.messages.onEmojiPick`.

use phoenix_ui::{
   Op,
   Prop,
   ViewKind,
};
use phoenix_wire::Value;

use super::{
   Patches,
   Place,
   bridge,
   commands,
   mirror::{
      Created,
      prop,
   },
   patches,
   press_context,
};

/// Puts what an `onEmojiPick` handler returned at the end of the message box,
/// or gives Phoenix the press back when none returned anything.
pub(super) fn pick(input: i32, id: &str, text: String, frame: Vec<u8>) -> Vec<Op> {
   let context = press_context(&Place::Screen);
   let Some(insert) = crate::engine().and_then(|engine| engine.pick_emoji(id, &context)) else {
      patches().resent.push(frame.clone());
      if !bridge::inject(&frame) {
         patches().resent.retain(|sent| *sent != frame);
         log::warn!("patches: the emoji picker did not take the press back");
      }
      return Vec::new();
   };
   let text = text + &insert;
   if !bridge::inject(&commands::change_text(input, &text)) {
      log::warn!("patches: the message box did not take the emoji");
   }
   patches().mirror.set_composer_text(input, text.clone());
   vec![commands::force_text(input, &text)]
}

const BACKGROUND: Prop = Prop::named("background");

impl Patches {
   /// Whether `id` is in the emoji picker under the message box (`xp-sheet`),
   /// not the one for reactions, which opens in a sheet of its own.
   pub(super) fn in_message_picker(&self, mut id: i32) -> bool {
      while let Some(parent) = self.mirror.parent(id) {
         if self.mirror.accessibility_id(parent) == Some("xp-sheet") {
            return true;
         }
         id = parent;
      }
      false
   }

   /// Hides the lock Phoenix draws over emojis it won't send, in the message
   /// box's picker: `ic_mana_lock` in a dimmed view a level or two inside the
   /// emoji. Presses on them go to the `onEmojiPick` handlers. Phoenix ignores
   /// `gone` there, so the icon loses its image and the view its dimming.
   pub(super) fn emoji_locks(&self, created: &Created<'_>) -> Vec<Op> {
      created
         .iter()
         .filter(|(_, (kind, props))| {
            **kind == ViewKind::Image
               && matches!(prop(props, Prop::URI), Some(Value::Str(uri)) if uri == "res://ic_mana_lock")
         })
         .filter_map(|(id, _)| {
            let overlay = self.mirror.parent(*id)?;
            let emoji = std::iter::successors(self.mirror.parent(overlay), |at| self.mirror.parent(*at))
               .take(3)
               .find(|at| {
                  self
                     .mirror
                     .accessibility_id(*at)
                     .is_some_and(|aid| aid.starts_with("expression.emoji:custom:"))
               })?;
            self.in_message_picker(emoji).then(|| {
               [
                  Op::Update {
                     id:    *id,
                     props: vec![(Prop::URI, Value::Str(String::new()))],
                  },
                  Op::Update {
                     id:    overlay,
                     props: vec![(BACKGROUND, Value::I32(0))],
                  },
               ]
            })
         })
         .flatten()
         .collect()
   }
}
