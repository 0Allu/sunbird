//! Custom emoji presses in the emoji picker under the message box, for
//! `phoenix.messages.onEmojiPick`, and Phoenix's emoji check, which while a
//! handler exists stops locking emojis that need Nitro in the message box.

use phoenix_hook::{
   Hook,
   Inline,
   pattern::Pattern,
};
use phoenix_ui::Op;

use crate::{
   bridge,
   patches::{
      Patches,
      Place,
      aids,
      commands,
      patches,
      worker::press_context,
   },
};

/// Puts what an `onEmojiPick` handler returned at the end of the message box,
/// or gives Phoenix the press back when none returned anything.
pub(super) fn pick(input: i32, id: &str, text: String, frame: &[u8]) -> Vec<Op> {
   let context = press_context(&Place::Screen);
   let Some(insert) = crate::engine().and_then(|engine| engine.pick_emoji(id, &context)) else {
      patches().resent.push(frame.to_owned());
      if !bridge::inject(frame) {
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
         if self.mirror.accessibility_id(parent) == Some(aids::EMOJI_SHEET) {
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
const UNAVAILABLE_REASON: Pattern = Pattern::new(
   "28 90 40 39 09 01 1F 32 3F 39 00 71 ?? ?? ?? 54 68 00 80 52 E0 03 08 2A C0 03 5F D6",
);
/// Thumb-2 start of the same check: `push {r4-r6,lr}; mov r4,r1; mov r5,r0`,
/// two wildcarded `bl`s, then the `0xFF` usable return.
#[cfg(target_arch = "arm")]
const UNAVAILABLE_REASON: Pattern =
   Pattern::new("70 B5 0C 46 05 46 ?? ?? ?? ?? C0 B2 20 B9 20 68 ?? ?? ?? ?? FF 20 70 BD");
/// `EmojiContext.intention`, and its value for the message box (`CHAT`).
#[cfg(target_arch = "aarch64")]
const INTENTION: usize = 0x24;
#[cfg(target_arch = "arm")]
const INTENTION: usize = 0x14;
#[cfg(any(target_arch = "aarch64", target_arch = "arm"))]
const CHAT: u8 = 3;
#[cfg(any(target_arch = "aarch64", target_arch = "arm"))]
const PREMIUM_LOCKED: u8 = 2;
#[cfg(any(target_arch = "aarch64", target_arch = "arm"))]
const USABLE: u8 = 0xFF;

#[cfg(any(target_arch = "aarch64", target_arch = "arm"))]
static UNLOCKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
#[cfg(any(target_arch = "aarch64", target_arch = "arm"))]
static CHECK: std::sync::OnceLock<Option<Inline>> = std::sync::OnceLock::new();

/// Hooks Phoenix's emoji check. Runs from the loader's constructor, before any
/// thread can be inside the check, because an arm32 hook rewrites 8 bytes of
/// it and a thread there would run the half-written jump.
pub(super) fn install() {
   #[cfg(any(target_arch = "aarch64", target_arch = "arm"))]
   CHECK.get_or_init(hook_check);
}

/// While `on`, emojis Phoenix keeps for Nitro count as usable in the message
/// box: the picker and autocomplete offer them without a lock. Reactions,
/// statuses and the rest keep Phoenix's answer, and nothing else about Nitro
/// changes.
pub(super) fn unlock(on: bool) {
   #[cfg(any(target_arch = "aarch64", target_arch = "arm"))]
   UNLOCKED.store(on, std::sync::atomic::Ordering::Release);
   #[cfg(not(any(target_arch = "aarch64", target_arch = "arm")))]
   let _ = on;
}

#[cfg(any(target_arch = "aarch64", target_arch = "arm"))]
fn hook_check() -> Option<Inline> {
   let image = &crate::STATE.get()?.image;
   let found = image.scan(UNAVAILABLE_REASON);
   let [target] = found.as_slice() else {
      log::warn!(
         "emoji check: {} matches, Nitro-locked emojis stay locked",
         found.len()
      );
      return None;
   };
   // ShadowHook picks ARM or Thumb by bit 0, and the arm32 pattern is Thumb.
   let target = if cfg!(target_arch = "arm") {
      *target | 1
   } else {
      *target
   };
   let target = std::ptr::NonNull::new(target as *mut std::ffi::c_void)?;
   let detour = std::ptr::NonNull::new(unavailable_reason as *mut std::ffi::c_void)?;
   // SAFETY: the pattern matches the start of the check, and the detour has
   // its ABI.
   match unsafe { Inline::attach(target, detour) } {
      Ok(hook) => Some(hook),
      Err(err) => {
         log::warn!("emoji check: {err}");
         None
      },
   }
}

#[cfg(any(target_arch = "aarch64", target_arch = "arm"))]
extern "C" fn unavailable_reason(emoji: *const u8, context: *const u8) -> u8 {
   // A call can land here before `CHECK` holds the hook that sent it.
   let Some(hook) = CHECK.wait() else {
      return USABLE;
   };
   // SAFETY: the trampoline runs the original check, which has this ABI.
   let original: extern "C" fn(*const u8, *const u8) -> u8 =
      unsafe { std::mem::transmute(hook.original().as_ptr()) };
   let reason = original(emoji, context);
   if reason != PREMIUM_LOCKED || !UNLOCKED.load(std::sync::atomic::Ordering::Acquire) {
      return reason;
   }
   // SAFETY: Phoenix passes a live context, and the intention byte is in it.
   let intention = unsafe { *context.add(INTENTION) };
   if intention == CHAT { USABLE } else { reason }
}
