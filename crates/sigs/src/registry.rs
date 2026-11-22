use std::collections::HashMap;

use crate::{
   Error,
   GATEWAY_HANDLER_FIELD,
   Image,
   table_entries,
};

/// Every Phoenix build has this event, so its entry seeds the walk.
const SEED: &str = "MESSAGE_CREATE";
const STRIDE: usize = 3 * size_of::<usize>();
const MAX_NAME: usize = 64;

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
pub struct GatewayRegistry {
   entries: Vec<GatewayEntry>,
   by_name: HashMap<String, EventId>,
}

impl GatewayRegistry {
   pub fn locate(image: &Image<'_>) -> Result<Self, Error> {
      let mut seeds = table_entries(image, SEED, GATEWAY_HANDLER_FIELD);
      seeds.sort_unstable();
      seeds.dedup();
      let seed = match seeds[..] {
         [(slot, _)] => slot,
         [] => {
            return Err(Error::NotFound {
               sig: "gateway registry".into(),
            });
         },
         _ => {
            return Err(Error::Ambiguous {
               sig:    "gateway registry".into(),
               anchor: format!("table {SEED:?}"),
               hits:   seeds.iter().map(|(slot, _)| *slot).collect(),
            });
         },
      };

      let mut first = seed;
      while let Some(previous) = first.checked_sub(STRIDE)
         && entry(image, previous).is_some()
      {
         first = previous;
      }

      let entries = (0..)
         .map(|index| first + index * STRIDE)
         .map_while(|slot| entry(image, slot))
         .collect::<Vec<GatewayEntry>>();

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
