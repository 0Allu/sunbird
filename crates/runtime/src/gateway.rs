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
   EventId,
   GatewayEvent,
   GatewayHandler,
   HandlerResult,
   RawStr,
};
use seq_macro::seq;

use crate::{
   Error,
   STATE,
   State,
   js::Before,
};

type HandlerFn = <GatewayHandler as Abi>::Fn;

struct Slot {
   detour:   HandlerFn,
   original: AtomicPtr<c_void>,
}

impl Slot {
   const fn new(detour: HandlerFn) -> Self {
      Self {
         detour,
         original: AtomicPtr::new(ptr::null_mut()),
      }
   }

   /// Waits out the window between `Inline::attach` returning and the
   /// trampoline being stored, since the hook is live as soon as it attaches.
   fn original(&self) -> HandlerFn {
      loop {
         let original = self.original.load(Ordering::Acquire);
         if !original.is_null() {
            // SAFETY: the backend returned a trampoline with the handler's ABI.
            break unsafe { std::mem::transmute::<*mut c_void, HandlerFn>(original) };
         }
         thread::yield_now();
      }
   }
}

/// Phoenix routes several events through one handler (`MESSAGE_DELETE` and
/// `MESSAGE_DELETE_BULK`), so every hooked handler needs a detour that knows
/// its own original.
static SLOTS: [Slot; 32] = seq!(N in 0..32 {
   [#(Slot::new(detour::<N>),)*]
});

thread_local! {
   #[allow(
      clippy::missing_const_for_thread_local,
      reason = "fires on Android only, where the initializer is already const"
   )]
   static IN_JS: Cell<bool> = const { Cell::new(false) };
}

#[derive(Default)]
pub struct Installed {
   by_event: HashMap<EventId, usize>,
   by_addr:  HashMap<usize, usize>,
   hooks:    Vec<Inline>,
}

/// Hooks the handler for `event` unless it is already hooked.
#[expect(
   clippy::significant_drop_tightening,
   reason = "the lock spans resolve and attach so two plugins cannot hook one handler twice"
)]
pub fn install(state: &State, event: EventId) -> Result<(), Error> {
   let mut installed = state
      .gateway
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
   if installed.by_event.contains_key(&event) {
      return Ok(());
   }

   let entry = state.registry.entry(event);
   let name = &entry.name;
   let sig = Build::gateway_sig(state.build, name);
   let located = sig.resolve(&state.image)?;
   let anchor = &sig.anchors[located.anchor];
   if located.addr != entry.handler {
      return Err(
         plumage_sigs::Error::Conflict {
            sig:      name.clone(),
            anchor:   "registry walk".into(),
            expected: located.addr,
            found:    entry.handler,
         }
         .into(),
      );
   }
   if let Some(&slot) = installed.by_addr.get(&located.addr) {
      log::info!(
         "{name} shares the handler at {:#x} with slot {slot}",
         located.addr
      );
      installed.by_event.insert(event, slot);
      return Ok(());
   }

   let index = installed.hooks.len();
   let slot = SLOTS.get(index).ok_or(Error::OutOfSlots(SLOTS.len()))?;
   let target = NonNull::new(located.addr as *mut c_void).expect("resolved addresses are non-null");
   let detour = NonNull::new(slot.detour as *mut c_void).expect("function pointers are non-null");
   // SAFETY: the rule resolved to a gateway handler entry, and every detour has
   // its ABI.
   let hook = unsafe { Inline::attach(target, detour) }?;
   slot
      .original
      .store(hook.original().as_ptr(), Ordering::Release);
   log::info!(
      "hooked {name} at {:#x} (image offset {:#x}) via `{anchor}`, {}/{} anchors agree",
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
   installed.by_addr.insert(located.addr, index);
   installed.by_event.insert(event, index);
   Ok(())
}

unsafe extern "C" fn detour<const SLOT: usize>(
   store: *mut c_void,
   event: *const GatewayEvent,
   ctx: *mut c_void,
) -> HandlerResult {
   let original = SLOTS[SLOT].original();
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

   let Some(id) = state.registry.id(name) else {
      return call(event);
   };
   let before = run_js(state, |engine| engine.before(id, name, data)).unwrap_or(Before::Keep);
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
   run_js(state, |engine| engine.after(id, name, data, result.is_ok()));
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
