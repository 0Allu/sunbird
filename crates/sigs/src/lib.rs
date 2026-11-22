//! Hook points as YARA-style rules, plus a table of known builds keyed by build
//! ID.
//!
//! A [`Sig`] lists [`Anchor`]s. Every anchor is evaluated, each one that
//! resolves must land on exactly one address, and all of them must agree. A
//! build-specific byte pattern and a portable table lookup can sit in one rule,
//! so a stale pattern is caught instead of hooked.
//!
//! Porting to a new Discord build is `cargo run -p sigscan --
//! liblibdiscord_wasm_host.so`. It evaluates the portable rules against the
//! file and prints a [`Build`] entry for [`BUILDS`]. Unknown builds still load
//! at runtime through [`Sig::gateway`].

use std::{
   borrow::Cow,
   ffi::c_void,
   fmt,
   marker::PhantomData,
   str::FromStr,
};

pub use plumage_hook::{
   BuildId,
   Image,
   Pattern,
   Segment,
};

mod builds;

pub use builds::BUILDS;

/// Libraries that carry the gateway registry, Phoenix's and the stand-in's.
pub const LIBRARIES: &[&str] = &["liblibdiscord_wasm_host.so", "libstand_in.so"];

/// Offset of the handler pointer in a gateway registry entry `{name ptr, name
/// len, handler}`.
pub const GATEWAY_HANDLER_FIELD: usize = 2 * size_of::<usize>();

/// Gateway dispatch events plumage knows the payload of.
///
/// The wire name is the registry key inside the target, so it only meets text
/// there and in sigscan's arguments. Plugins hold an opaque handle per event,
/// `plumage.gateway[js_name]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Dispatch {
   MessageCreate,
   MessageUpdate,
   MessageDelete,
   MessageDeleteBulk,
}

impl Dispatch {
   pub const ALL: &[Self] = &[
      Self::MessageCreate,
      Self::MessageUpdate,
      Self::MessageDelete,
      Self::MessageDeleteBulk,
   ];

   #[must_use]
   pub const fn js_name(self) -> &'static str {
      match self {
         Self::MessageCreate => "messageCreate",
         Self::MessageUpdate => "messageUpdate",
         Self::MessageDelete => "messageDelete",
         Self::MessageDeleteBulk => "messageDeleteBulk",
      }
   }

   #[must_use]
   pub const fn wire(self) -> &'static str {
      match self {
         Self::MessageCreate => "MESSAGE_CREATE",
         Self::MessageUpdate => "MESSAGE_UPDATE",
         Self::MessageDelete => "MESSAGE_DELETE",
         Self::MessageDeleteBulk => "MESSAGE_DELETE_BULK",
      }
   }
}

impl fmt::Display for Dispatch {
   fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
      formatter.write_str(self.wire())
   }
}

#[derive(Debug, thiserror::Error)]
#[error("`{0}` is not a gateway event plumage knows")]
pub struct UnknownDispatch(String);

impl FromStr for Dispatch {
   type Err = UnknownDispatch;

   fn from_str(name: &str) -> Result<Self, Self::Err> {
      Self::ALL
         .iter()
         .copied()
         .find(|event| event.wire() == name)
         .ok_or_else(|| UnknownDispatch(name.to_owned()))
   }
}

/// One way to find a hook point.
#[derive(Clone, Debug)]
pub enum Anchor<'a> {
   /// Code matching `pattern`, shifted by `offset`.
   Code { pattern: Pattern, offset: isize },
   /// A `{name ptr, name len, ...}` table entry whose name is `key`. The result
   /// is the code pointer `field` bytes into the entry.
   Table { key: Cow<'a, str>, field: usize },
}

impl fmt::Display for Anchor<'_> {
   fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
      match self {
         Self::Code { pattern, offset: 0 } => write!(formatter, "code {pattern}"),
         Self::Code { pattern, offset } => write!(formatter, "code {pattern} {offset:+}"),
         Self::Table { key, field } => write!(formatter, "table {key:?} +{field}"),
      }
   }
}

/// Calling convention of a hook point, which picks the detour the runtime
/// installs.
pub trait Abi {
   type Fn: Copy;
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RawStr {
   pub ptr: *const u8,
   pub len: usize,
}

/// Phoenix's gateway event, `name` and `data` are borrowed UTF-8 with `data`
/// holding the raw dispatch JSON. `rest` is opaque and copied through
/// unchanged.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct GatewayEvent {
   pub name: RawStr,
   pub data: RawStr,
   pub rest: [u64; 4],
}

/// A 32-byte Rust `Result`, returned through x8 on arm64 and a hidden pointer
/// on x86_64.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct HandlerResult(pub [u64; 4]);

impl HandlerResult {
   pub const OK: Self = Self([u64::MAX, 0, 0, 0]);

   #[must_use]
   pub const fn is_ok(&self) -> bool {
      self.0[0] == u64::MAX
   }
}

/// A gateway registry handler, `handler(store, event, ctx)`.
pub struct GatewayHandler;

impl Abi for GatewayHandler {
   type Fn = unsafe extern "C" fn(*mut c_void, *const GatewayEvent, *mut c_void) -> HandlerResult;
}

#[derive(Debug)]
pub struct Sig<'a, A: Abi> {
   pub name:    Cow<'a, str>,
   pub anchors: Cow<'a, [Anchor<'a>]>,
   abi:         PhantomData<fn() -> A>,
}

impl<A: Abi> Clone for Sig<'_, A> {
   fn clone(&self) -> Self {
      Self {
         name:    self.name.clone(),
         anchors: self.anchors.clone(),
         abi:     PhantomData,
      }
   }
}

impl<'a, A: Abi> Sig<'a, A> {
   #[must_use]
   pub const fn new(name: &'a str, anchors: &'a [Anchor<'a>]) -> Self {
      Self {
         name:    Cow::Borrowed(name),
         anchors: Cow::Borrowed(anchors),
         abi:     PhantomData,
      }
   }
}

impl Sig<'static, GatewayHandler> {
   /// The portable rule for a gateway event, a registry lookup by its wire
   /// name.
   #[must_use]
   pub fn gateway(event: Dispatch) -> Self {
      Self {
         name:    Cow::Borrowed(event.wire()),
         anchors: Cow::Owned(vec![Anchor::Table {
            key:   Cow::Borrowed(event.wire()),
            field: GATEWAY_HANDLER_FIELD,
         }]),
         abi:     PhantomData,
      }
   }
}

/// Where a rule resolved, which anchor found it first, and how many anchors
/// agreed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Located {
   pub addr:     usize,
   pub anchor:   usize,
   pub agreeing: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
   #[error("{sig}: no anchor matched")]
   NotFound { sig: String },
   #[error("{sig}: `{anchor}` matched {} places ({})", hits.len(), hex_list(hits))]
   Ambiguous {
      sig:    String,
      anchor: String,
      hits:   Vec<usize>,
   },
   #[error("{sig}: `{anchor}` found {found:#x}, an earlier anchor found {expected:#x}")]
   Conflict {
      sig:      String,
      anchor:   String,
      expected: usize,
      found:    usize,
   },
}

fn hex_list(addrs: &[usize]) -> String {
   addrs
      .iter()
      .take(8)
      .map(|addr| format!("{addr:#x}"))
      .collect::<Vec<_>>()
      .join(", ")
}

impl<A: Abi> Sig<'_, A> {
   pub fn resolve(&self, image: &Image<'_>) -> Result<Located, Error> {
      let mut located: Option<Located> = None;
      for (index, anchor) in self.anchors.iter().enumerate() {
         let mut hits = evaluate(anchor, image);
         hits.sort_unstable();
         hits.dedup();
         let found = match hits[..] {
            [] => continue,
            [found] => found,
            _ => {
               return Err(Error::Ambiguous {
                  sig: self.name.to_string(),
                  anchor: anchor.to_string(),
                  hits,
               });
            },
         };
         match &mut located {
            None => {
               located = Some(Located {
                  addr:     found,
                  anchor:   index,
                  agreeing: 1,
               });
            },
            Some(first) if first.addr != found => {
               return Err(Error::Conflict {
                  sig: self.name.to_string(),
                  anchor: anchor.to_string(),
                  expected: first.addr,
                  found,
               });
            },
            Some(first) => first.agreeing += 1,
         }
      }
      located.ok_or_else(|| {
         Error::NotFound {
            sig: self.name.to_string(),
         }
      })
   }
}

fn evaluate(anchor: &Anchor<'_>, image: &Image<'_>) -> Vec<usize> {
   match anchor {
      Anchor::Code { pattern, offset } => {
         image
            .scan(*pattern)
            .into_iter()
            .filter_map(|addr| addr.checked_add_signed(*offset))
            .collect()
      },
      Anchor::Table { key, field } => {
         image
            .find_bytes(key.as_bytes())
            .into_iter()
            .flat_map(|string| image.pointers_to(string))
            .filter(|slot| image.read_usize(slot + size_of::<usize>()) == Some(key.len()))
            .filter_map(|slot| image.read_usize(slot + field))
            .filter(|code| image.is_function(*code))
            .collect()
      },
   }
}

/// A gateway handler rule in a [`Build`].
pub struct Gateway {
   pub event: Dispatch,
   pub sig:   Sig<'static, GatewayHandler>,
}

/// A build plumage has rules for.
pub struct Build {
   pub id:      BuildId,
   pub label:   &'static str,
   pub library: &'static str,
   pub gateway: &'static [Gateway],
}

impl Build {
   #[must_use]
   pub fn find(id: BuildId) -> Option<&'static Self> {
      BUILDS.iter().find(|build| build.id == id)
   }

   /// The table rule for `event`, or the portable registry lookup when the
   /// table has none.
   #[must_use]
   pub fn gateway_sig(build: Option<&Self>, event: Dispatch) -> Sig<'static, GatewayHandler> {
      build
         .and_then(|build| build.gateway.iter().find(|rule| rule.event == event))
         .map_or_else(|| Sig::gateway(event), |rule| rule.sig.clone())
   }
}
