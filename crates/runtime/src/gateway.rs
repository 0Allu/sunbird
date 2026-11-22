use std::{
   cell::Cell,
   collections::HashMap,
   ffi::c_void,
   panic::{
      self,
      AssertUnwindSafe,
   },
   ptr::{
      self,
      NonNull,
   },
   slice,
   str,
   sync::atomic::{
      AtomicPtr,
      Ordering,
   },
   thread,
};

use plumage_hook::{
   Hook,
   Inline,
};
use plumage_sigs::{
   Abi,
   Build,
   Dispatch,
   GatewayEvent,
   GatewayHandler,
   HandlerResult,
   RawStr,
};

use crate::{
   Error,
   STATE,
   State,
   js::Before,
};

type HandlerFn = <GatewayHandler as Abi>::Fn;

const SLOTS: usize = 32;

/// Trampolines to the original handlers, indexed like [`DETOURS`].
static ORIGINALS: [AtomicPtr<c_void>; SLOTS] = [const { AtomicPtr::new(ptr::null_mut()) }; SLOTS];

macro_rules! detours {
   ($($slot:literal)*) => { [$(detour::<$slot> as HandlerFn,)*] };
}

/// One detour per slot, so each knows its own original even when Phoenix routes
/// several events through one handler (`MESSAGE_DELETE` and
/// `MESSAGE_DELETE_BULK`).
static DETOURS: [HandlerFn; SLOTS] =
   detours!(0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30 31);

thread_local! {
   #[allow(
      clippy::missing_const_for_thread_local,
      reason = "fires on Android only, where the initializer is already const"
   )]
   static IN_JS: Cell<bool> = const { Cell::new(false) };
}

#[derive(Default)]
pub struct Installed {
   by_event: HashMap<Dispatch, usize>,
   by_addr:  HashMap<usize, usize>,
   hooks:    Vec<Inline>,
}

/// Hooks the handler for `event` unless it is already hooked.
#[expect(
   clippy::significant_drop_tightening,
   reason = "the lock spans resolve and attach so two plugins cannot hook one handler twice"
)]
pub fn install(state: &State, event: Dispatch) -> Result<(), Error> {
   let mut installed = state
      .gateway
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
   if installed.by_event.contains_key(&event) {
      return Ok(());
   }

   let sig = Build::gateway_sig(state.build, event);
   let located = sig.resolve(&state.image)?;
   let anchor = &sig.anchors[located.anchor];
   if let Some(&slot) = installed.by_addr.get(&located.addr) {
      log::info!(
         "{event} shares the handler at {:#x} with slot {slot}",
         located.addr
      );
      installed.by_event.insert(event, slot);
      return Ok(());
   }

   let slot = installed.hooks.len();
   if slot == SLOTS {
      return Err(Error::OutOfSlots(SLOTS));
   }
   let target = NonNull::new(located.addr as *mut c_void).expect("resolved addresses are non-null");
   let detour = NonNull::new(DETOURS[slot] as *mut c_void).expect("function pointers are non-null");
   // SAFETY: the rule resolved to a gateway handler entry, and every detour has
   // its ABI.
   let hook = unsafe { Inline::attach(target, detour) }?;
   ORIGINALS[slot].store(hook.original().as_ptr(), Ordering::Release);
   log::info!(
      "hooked {event} at {:#x} (image offset {:#x}) via `{anchor}`, {}/{} anchors agree",
      located.addr,
      located.addr
         - state
            .image
            .segments
            .first()
            .map_or(0, |segment| segment.addr),
      located.agreeing,
      sig.anchors.len(),
   );
   installed.hooks.push(hook);
   installed.by_addr.insert(located.addr, slot);
   installed.by_event.insert(event, slot);
   Ok(())
}

unsafe extern "C" fn detour<const SLOT: usize>(
   store: *mut c_void,
   event: *const GatewayEvent,
   ctx: *mut c_void,
) -> HandlerResult {
   // A hook can fire between attach returning and the store above.
   let original = loop {
      let original = ORIGINALS[SLOT].load(Ordering::Acquire);
      if !original.is_null() {
         // SAFETY: the backend returned a trampoline with the handler's ABI.
         break unsafe { std::mem::transmute::<*mut c_void, HandlerFn>(original) };
      }
      thread::yield_now();
   };
   // SAFETY: the target passes a live event for the duration of the call.
   let event = unsafe { &*event };
   // SAFETY: as above, forwarded to the original unchanged or with replaced
   // data.
   let call = |event: &GatewayEvent| unsafe { original(store, event, ctx) };

   let (Some(state), false) = (STATE.get(), IN_JS.get()) else {
      return call(event);
   };
   // SAFETY: name and data are borrowed strings that outlive the call.
   let (Some(name), Some(data)) = (unsafe { text(event.name) }, unsafe { text(event.data) }) else {
      return call(event);
   };

   let Ok(dispatch) = name.parse::<Dispatch>() else {
      return call(event);
   };
   let before = run_js(state, |engine| engine.before(dispatch, data)).unwrap_or(Before::Keep);
   let (result, data) = match &before {
      Before::Drop => return HandlerResult::OK,
      Before::Keep => (call(event), data),
      Before::Replace(json) => {
         let replaced = GatewayEvent {
            data: RawStr {
               ptr: json.as_ptr(),
               len: json.len(),
            },
            ..*event
         };
         (call(&replaced), json.as_str())
      },
   };
   run_js(state, |engine| engine.after(dispatch, data, result.is_ok()));
   result
}

/// # Safety
///
/// `raw` describes live bytes.
unsafe fn text<'a>(raw: RawStr) -> Option<&'a str> {
   if raw.ptr.is_null() {
      return None;
   }
   // SAFETY: forwarded from the caller.
   str::from_utf8(unsafe { slice::from_raw_parts(raw.ptr, raw.len) }).ok()
}

/// Runs `work` on the engine, logging JS errors and panics instead of unwinding
/// into the host. Nested gateway calls made while JS runs go straight to the
/// original.
fn run_js<T>(
   state: &State,
   work: impl FnOnce(&mut crate::js::Engine) -> Result<T, Error>,
) -> Option<T> {
   IN_JS.set(true);
   let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
      let mut engine = state
         .js
         .lock()
         .unwrap_or_else(std::sync::PoisonError::into_inner);
      work(&mut engine)
   }));
   IN_JS.set(false);
   match outcome {
      Ok(Ok(value)) => Some(value),
      Ok(Err(err)) => {
         log::error!("{err}");
         None
      },
      Err(_) => {
         log::error!("a gateway hook panicked, the event went through unmodified");
         None
      },
   }
}
