//! Custom emoji presses in the emoji picker under the message box, for
//! `phoenix.messages.onEmojiPick`, and Phoenix's emoji check, which while a
//! handler exists stops locking emojis that need Nitro in the message box.

use phoenix_ui::Op;

use super::{
   Patches,
   Place,
   bridge,
   commands,
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
}

/// Phoenix's `getEmojiUnavailableReason`: `u8 f(&Emoji, &EmojiContext)`, where
/// 0xFF means usable and 2 premium locked. Found by its start, which reads the
/// context's intention and answers 3 for profiles.
#[cfg(target_arch = "aarch64")]
const UNAVAILABLE_REASON: phoenix_hook::Pattern = phoenix_hook::Pattern::ida(
   "28 90 40 39 09 01 1F 32 3F 39 00 71 ?? ?? ?? 54 68 00 80 52 E0 03 08 2A C0 03 5F D6",
);
/// `EmojiContext.intention`, and its value for the message box (`CHAT`).
#[cfg(target_arch = "aarch64")]
const INTENTION: usize = 0x24;
#[cfg(target_arch = "aarch64")]
const CHAT: u8 = 3;
#[cfg(target_arch = "aarch64")]
const PREMIUM_LOCKED: u8 = 2;
#[cfg(target_arch = "aarch64")]
const USABLE: u8 = 0xFF;

#[cfg(target_arch = "aarch64")]
static UNLOCKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
#[cfg(target_arch = "aarch64")]
static CHECK: std::sync::OnceLock<Option<phoenix_hook::Inline>> = std::sync::OnceLock::new();

/// While `on`, emojis Phoenix keeps for Nitro count as usable in the message
/// box: the picker and autocomplete offer them without a lock. Reactions,
/// statuses and the rest keep Phoenix's answer, and nothing else about Nitro
/// changes. The check is found and hooked once, off the calling thread.
pub(super) fn unlock(on: bool) {
   #[cfg(target_arch = "aarch64")]
   {
      UNLOCKED.store(on, std::sync::atomic::Ordering::Release);
      if on && CHECK.get().is_none() {
         std::thread::spawn(|| {
            CHECK.get_or_init(hook_check);
         });
      }
   }
   #[cfg(not(target_arch = "aarch64"))]
   let _ = on;
}

#[cfg(target_arch = "aarch64")]
fn hook_check() -> Option<phoenix_hook::Inline> {
   use phoenix_hook::Hook;

   let image = &crate::STATE.get()?.image;
   let found = image.scan(UNAVAILABLE_REASON);
   let [target] = found.as_slice() else {
      log::warn!(
         "emoji check: {} matches, Nitro-locked emojis stay locked",
         found.len()
      );
      return None;
   };
   let target = std::ptr::NonNull::new(*target as *mut std::ffi::c_void)?;
   let detour = std::ptr::NonNull::new(unavailable_reason as *mut std::ffi::c_void)?;
   // SAFETY: the pattern matches the start of the check, and the detour has
   // its ABI.
   match unsafe { phoenix_hook::Inline::attach(target, detour) } {
      Ok(hook) => Some(hook),
      Err(err) => {
         log::warn!("emoji check: {err}");
         None
      },
   }
}

#[cfg(target_arch = "aarch64")]
extern "C" fn unavailable_reason(emoji: *const u8, context: *const u8) -> u8 {
   use phoenix_hook::Hook;

   // A call can land here before `CHECK` holds the hook that sent it.
   let Some(hook) = CHECK.wait() else {
      return USABLE;
   };
   // SAFETY: the trampoline runs the original check, which has this ABI.
   let original: extern "C" fn(*const u8, *const u8) -> u8 =
      unsafe { std::mem::transmute(hook.original().as_ptr()) };
   let reason = original(emoji, context);
   // SAFETY: Phoenix passes a live context, and the intention byte is in it.
   let intention = unsafe { *context.add(INTENTION) };
   if reason == PREMIUM_LOCKED
      && intention == CHAT
      && UNLOCKED.load(std::sync::atomic::Ordering::Acquire)
   {
      USABLE
   } else {
      reason
   }
}
