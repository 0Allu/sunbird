use std::collections::HashMap;

use phoenix_hook::image::Image;

use crate::GATEWAY_HANDLER_FIELD;

/// Every Phoenix build has this event, so its entry seeds the walk.
const SEED: &str = "MESSAGE_CREATE";
/// Seeds the walk over the reducers behind the exported
/// `LIBDISCORD_REDUCERS_TABLE`, whose per-domain slices sit back to back.
const DOMAIN_SEED: &str = "READY_SUPPLEMENTAL";
const STRIDE: usize = 3 * size_of::<usize>();
const MAX_NAME: usize = 64;

#[derive(Debug, flub::Error)]
pub enum Error {
   #[error("no gateway table holds {seed}")]
   NotFound { seed: &'static str },
   #[error("{} gateway tables hold {seed}", .slots.len())]
   Ambiguous {
      seed:  &'static str,
      slots: Vec<usize>,
   },
}

/// A gateway event that exists in the loaded target. Only
/// [`GatewayRegistry`] makes these, so holding one proves the event is live.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EventId(usize);

impl EventId {
   /// Position in [`GatewayRegistry::iter`] order.
   #[must_use]
   pub const fn index(self) -> usize {
      self.0
   }
}

#[derive(Clone, Debug)]
pub struct GatewayEntry {
   pub name:    String,
   pub slot:    usize,
   pub handler: usize,
}

/// The target's gateway registry, a contiguous array of `{name ptr, name len,
/// handler}`, walked out from the [`SEED`] entry in both directions until an
/// entry stops looking like one.
///
/// The domain reducers, found from [`DOMAIN_SEED`] the same way, follow when
/// the target has them.
pub struct GatewayRegistry {
   entries: Vec<GatewayEntry>,
   by_name: HashMap<String, EventId>,
}

impl GatewayRegistry {
   pub fn locate(image: &Image<'_>) -> Result<Self, Error> {
      let mut entries = walk(image, SEED)?;
      match walk(image, DOMAIN_SEED) {
         Ok(domain) => {
            let main = entries.len();
            for entry in domain {
               if !entries[..main].iter().any(|known| known.name == entry.name) {
                  entries.push(entry);
               }
            }
         },
         Err(Error::NotFound { .. }) => {},
         Err(err) => return Err(err),
      }

      let by_name = entries
         .iter()
         .enumerate()
         .map(|(index, entry)| (entry.name.clone(), EventId(index)))
         .collect();

      Ok(Self { entries, by_name })
   }

   #[must_use]
   pub fn id(&self, name: &str) -> Option<EventId> {
      self.by_name.get(name).copied()
   }

   /// # Panics
   ///
   /// When `id` came from another registry, which one process never has.
   #[must_use]
   pub fn entry(&self, id: EventId) -> &GatewayEntry {
      &self.entries[id.0]
   }

   pub fn iter(&self) -> impl Iterator<Item = (EventId, &GatewayEntry)> {
      self
         .entries
         .iter()
         .enumerate()
         .map(|(index, entry)| (EventId(index), entry))
   }
}

fn entry(image: &Image<'_>, slot: usize) -> Option<GatewayEntry> {
   let name = image.read_usize(slot)?;
   let len = image.read_usize(slot + size_of::<usize>())?;
   let handler = image.read_usize(slot + GATEWAY_HANDLER_FIELD)?;
   if !(1..=MAX_NAME).contains(&len) || !image.is_function(handler) {
      return None;
   }
   let name = image.read(name, len)?;
   if !name
      .iter()
      .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || *byte == b'_')
   {
      return None;
   }
   Some(GatewayEntry {
      name: String::from_utf8(name.to_vec()).ok()?,
      slot,
      handler,
   })
}

fn walk(image: &Image<'_>, seed: &'static str) -> Result<Vec<GatewayEntry>, Error> {
   let mut seeds = table_entries(image, seed, GATEWAY_HANDLER_FIELD);
   seeds.sort_unstable();
   seeds.dedup();
   let start = match seeds[..] {
      [(slot, _)] => slot,
      [] => return Err(Error::NotFound { seed }),
      _ => {
         return Err(Error::Ambiguous {
            seed,
            slots: seeds.iter().map(|(slot, _)| *slot).collect(),
         });
      },
   };

   let mut first = start;
   while let Some(previous) = first.checked_sub(STRIDE)
      && entry(image, previous).is_some()
   {
      first = previous;
   }

   Ok((0..)
      .map(|index| first + index * STRIDE)
      .map_while(|slot| entry(image, slot))
      .collect())
}

/// `(slot, code)` for every `{name ptr, name len, ...}` entry named `key` whose
/// pointer `field` bytes in starts a function.
fn table_entries(image: &Image<'_>, key: &str, field: usize) -> Vec<(usize, usize)> {
   // Stops at the first copy of `key` a table points at. Literals sit in
   // `.rodata` at the front of the image, so the scan never reaches `.text`
   // (most of the library) at startup. Every table pointing at that copy still
   // counts, so callers still see ambiguity.
   image
      .find_bytes_iter(key.as_bytes())
      .map(|string| {
         image
            .pointers_to(string)
            .into_iter()
            .filter(|slot| image.read_usize(slot + size_of::<usize>()) == Some(key.len()))
            .filter_map(|slot| Some((slot, image.read_usize(slot + field)?)))
            .filter(|(_, code)| image.is_function(*code))
            .collect::<Vec<_>>()
      })
      .find(|entries| !entries.is_empty())
      .unwrap_or_default()
}
