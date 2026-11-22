//! Typed codec for Phoenix UI-op batches (`GuestHost.applyOps`) and the
//! view-event frames Java sends back through `WasmRuntime.nativeDispatch`, plus
//! Phoenix's own settings layouts.
//!
//! All integers are little-endian. A batch is a version byte followed by ops
//! until the end of the buffer.

use std::{
   fmt,
   str::{
      FromStr,
      Utf8Error,
   },
   sync::OnceLock,
};

use serde::Deserialize;

/// The prop and event name table (`lh/c2.a`, versionCode 356207). The index is
/// the wire key.
pub const PROPS: [&str; 855] = include!("props.in");

const VERSION: u8 = 1;
const MAX_DEPTH: usize = 128;

/// A prop or event key. Keys past [`PROPS`] decode from the wire and have no
/// name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Prop(u16);

impl Prop {
   pub const ACCESSIBILITY_CHECKED: Self = Self::named("accessibilityChecked");
   pub const ACCESSIBILITY_ID: Self = Self::named("accessibilityId");
   pub const ACCESSIBILITY_LABEL: Self = Self::named("accessibilityLabel");
   pub const CHANGE_TEXT: Self = Self::named("changeText");
   pub const COLOR: Self = Self::named("color");
   pub const FONT_FAMILY: Self = Self::named("fontFamily");
   pub const FONT_SIZE: Self = Self::named("fontSize");
   pub const GAP: Self = Self::named("gap");
   pub const HEIGHT: Self = Self::named("height");
   pub const INDEX: Self = Self::named("index");
   pub const KEYBOARD: Self = Self::named("keyboard");
   pub const LINE_HEIGHT: Self = Self::named("lineHeight");
   pub const MARGIN: Self = Self::named("margin");
   pub const NAVIGATE: Self = Self::named("navigate");
   pub const ON: Self = Self::named("on");
   pub const PADDING: Self = Self::named("padding");
   pub const PANE_TITLE: Self = Self::named("paneTitle");
   pub const PLACEHOLDER: Self = Self::named("placeholder");
   pub const PRESENTATION: Self = Self::named("presentation");
   pub const PRESS: Self = Self::named("press");
   pub const RADIUS: Self = Self::named("radius");
   pub const RADIUS_BOTTOM_LEFT: Self = Self::named("radiusBottomLeft");
   pub const RADIUS_BOTTOM_RIGHT: Self = Self::named("radiusBottomRight");
   pub const RADIUS_TOP_LEFT: Self = Self::named("radiusTopLeft");
   pub const RADIUS_TOP_RIGHT: Self = Self::named("radiusTopRight");
   pub const RETURN_KEY: Self = Self::named("returnKey");
   pub const ROLE: Self = Self::named("role");
   pub const SELECTABLE: Self = Self::named("selectable");
   pub const STACK_HEADER: Self = Self::named("stackHeader");
   pub const TEXT: Self = Self::named("text");
   pub const TINT: Self = Self::named("tint");
   pub const UIKIT_PUSH: Self = Self::named("uikitPush");
   pub const URI: Self = Self::named("uri");
   pub const VALUE: Self = Self::named("value");

   /// Looks up `name` in [`PROPS`]. A missing name fails const evaluation.
   #[must_use]
   pub const fn named(name: &str) -> Self {
      let mut key = 0;
      while key < PROPS.len() {
         if bytes_eq(PROPS[key].as_bytes(), name.as_bytes()) {
            #[expect(clippy::cast_possible_truncation, reason = "PROPS has 855 entries")]
            return Self(key as u16);
         }
         key += 1;
      }
      panic!("name is not in PROPS");
   }

   #[must_use]
   pub const fn from_key(key: u16) -> Self {
      Self(key)
   }

   #[must_use]
   pub const fn key(self) -> u16 {
      self.0
   }

   #[must_use]
   pub fn name(self) -> Option<&'static str> {
      PROPS.get(usize::from(self.0)).copied()
   }
}

const fn bytes_eq(left: &[u8], right: &[u8]) -> bool {
   if left.len() != right.len() {
      return false;
   }

   let mut idx = 0;
   while idx < left.len() {
      if left[idx] != right[idx] {
         return false;
      }
      idx += 1;
   }
   true
}

#[derive(Debug, flub::Error)]
#[error("unknown prop {0:?}")]
pub struct UnknownProp(pub String);

impl FromStr for Prop {
   type Err = UnknownProp;

   fn from_str(name: &str) -> Result<Self, Self::Err> {
      PROPS
         .iter()
         .position(|known| *known == name)
         .and_then(|key| u16::try_from(key).ok())
         .map(Self)
         .ok_or_else(|| UnknownProp(name.to_owned()))
   }
}

impl fmt::Display for Prop {
   fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
      match self.name() {
         Some(name) => formatter.write_str(name),
         None => write!(formatter, "{}", self.0),
      }
   }
}

/// A tagged value (`yq.f`).
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
   Null,
   Bool(bool),
   I32(i32),
   I64(i64),
   F64(f64),
   Str(String),
   Arr(Vec<Self>),
   Obj(Vec<(Prop, Self)>),
   /// Padding and margin insets as `[left, top, right, bottom]`.
   Rect([f32; 4]),
   Map(Vec<(String, Self)>),
   Words(Vec<i32>),
   /// Copies list `list` from this batch's words table, sets each `(index,
   /// word)`, and appends the result, so it never moves to another batch.
   WPatch {
      list: i32,
      set:  Vec<(u16, i32)>,
   },
}

pub type Props = Vec<(Prop, Value)>;

#[derive(Clone, Debug, PartialEq)]
pub enum Op {
   Create {
      id:    i32,
      kind:  String,
      props: Props,
   },
   Update {
      id:    i32,
      props: Props,
   },
   Children {
      id:  i32,
      ids: Vec<i32>,
   },
   Delete {
      ids: Vec<i32>,
   },
   Root {
      id: i32,
   },
   Focus {
      id: Option<i32>,
   },
   ScrollTo {
      id:    i32,
      value: Value,
   },
   RequestFrame,
   Motion {
      words: Vec<i32>,
   },
   ChildEdits {
      id:    i32,
      edits: Vec<ChildEdit>,
   },
}

/// One cursor step over the parent's cached child list: copy `keep`, skip
/// `del`, append `ins`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChildEdit {
   pub keep: i32,
   pub del:  i32,
   pub ins:  Vec<i32>,
}

#[derive(Debug, flub::Error)]
pub enum DecodeError {
   #[error("view wire version {0}, expected 1")]
   Version(u8),
   #[error("input ends early at byte {at}")]
   Underflow { at: usize },
   #[error("negative count {count} at byte {at}")]
   NegativeCount { at: usize, count: i32 },
   #[error("unknown op {op} at byte {at}")]
   UnknownOp { at: usize, op: u8 },
   #[error("unknown value tag {tag} at byte {at}")]
   UnknownTag { at: usize, tag: u8 },
   #[error("invalid utf-8 at byte {at}")]
   Utf8 {
      at:     usize,
      #[source]
      source: Utf8Error,
   },
   #[error("nesting deeper than {MAX_DEPTH} at byte {at}")]
   TooDeep { at: usize },
   #[error("frame at byte {at} runs past its batch")]
   FrameOverrun { at: usize },
}

struct Reader<'buf> {
   bytes: &'buf [u8],
   pos:   usize,
}

impl<'buf> Reader<'buf> {
   const fn at_end(&self) -> bool {
      self.pos >= self.bytes.len()
   }

   fn take(&mut self, len: usize) -> Result<&'buf [u8], DecodeError> {
      let slice = self
         .bytes
         .get(self.pos..)
         .and_then(|rest| rest.get(..len))
         .ok_or(DecodeError::Underflow { at: self.pos })?;

      self.pos += len;
      Ok(slice)
   }

   fn chunk<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
      let chunk = self
         .bytes
         .get(self.pos..)
         .and_then(<[u8]>::first_chunk::<N>)
         .ok_or(DecodeError::Underflow { at: self.pos })?;

      self.pos += N;
      Ok(*chunk)
   }

   fn u8(&mut self) -> Result<u8, DecodeError> {
      self.chunk().map(u8::from_le_bytes)
   }

   fn u16(&mut self) -> Result<u16, DecodeError> {
      self.chunk().map(u16::from_le_bytes)
   }

   fn i32(&mut self) -> Result<i32, DecodeError> {
      self.chunk().map(i32::from_le_bytes)
   }

   fn count(&mut self) -> Result<usize, DecodeError> {
      let at = self.pos;
      let count = self.i32()?;
      usize::try_from(count).map_err(|_| DecodeError::NegativeCount { at, count })
   }

   fn str(&mut self, len: usize) -> Result<String, DecodeError> {
      let at = self.pos;
      let bytes = self.take(len)?;
      std::str::from_utf8(bytes)
         .map(str::to_owned)
         .map_err(|source| DecodeError::Utf8 { at, source })
   }

   fn ints(&mut self) -> Result<Vec<i32>, DecodeError> {
      (0..self.count()?).map(|_| self.i32()).collect()
   }

   fn value(&mut self, depth: usize) -> Result<Value, DecodeError> {
      let at = self.pos;
      if depth > MAX_DEPTH {
         return Err(DecodeError::TooDeep { at });
      }

      let value = match self.u8()? {
         0 => Value::Null,
         1 => Value::Bool(false),
         2 => Value::Bool(true),
         3 => Value::I32(self.i32()?),
         4 => Value::I64(self.chunk().map(i64::from_le_bytes)?),
         5 => Value::F64(self.chunk().map(f64::from_le_bytes)?),
         6 => {
            let len = self.count()?;
            Value::Str(self.str(len)?)
         },
         7 => {
            Value::Arr(
               (0..self.count()?)
                  .map(|_| self.value(depth + 1))
                  .collect::<Result<_, _>>()?,
            )
         },
         8 => Value::Obj(self.entries(depth + 1, Self::count)?),
         9 => {
            Value::Rect([
               self.chunk().map(f32::from_le_bytes)?,
               self.chunk().map(f32::from_le_bytes)?,
               self.chunk().map(f32::from_le_bytes)?,
               self.chunk().map(f32::from_le_bytes)?,
            ])
         },
         10 => {
            let pairs = (0..self.count()?)
               .map(|_| {
                  let len = self.count()?;
                  Ok((self.str(len)?, self.value(depth + 1)?))
               })
               .collect::<Result<_, DecodeError>>()?;

            Value::Map(pairs)
         },
         11 => Value::Words(self.ints()?),
         12 => {
            let list = self.i32()?;
            let set = (0..self.u16()?)
               .map(|_| Ok((self.u16()?, self.i32()?)))
               .collect::<Result<_, DecodeError>>()?;

            Value::WPatch { list, set }
         },
         tag => return Err(DecodeError::UnknownTag { at, tag }),
      };
      Ok(value)
   }

   fn entries(
      &mut self,
      depth: usize,
      count: fn(&mut Self) -> Result<usize, DecodeError>,
   ) -> Result<Props, DecodeError> {
      (0..count(self)?)
         .map(|_| Ok((Prop(self.u16()?), self.value(depth)?)))
         .collect()
   }

   fn props(&mut self) -> Result<Props, DecodeError> {
      self.entries(0, |reader| reader.u16().map(usize::from))
   }
}

pub fn decode(bytes: &[u8]) -> Result<Vec<Op>, DecodeError> {
   let mut reader = Reader { bytes, pos: 0 };
   let version = reader.u8()?;
   if version != VERSION {
      return Err(DecodeError::Version(version));
   }

   let mut ops = Vec::new();
   while !reader.at_end() {
      let at = reader.pos;
      let op = match reader.u8()? {
         1 => {
            let id = reader.i32()?;
            let len = reader.u16()?;
            let kind = reader.str(usize::from(len))?;
            Op::Create {
               id,
               kind,
               props: reader.props()?,
            }
         },
         2 => {
            Op::Update {
               id:    reader.i32()?,
               props: reader.props()?,
            }
         },
         3 => {
            Op::Children {
               id:  reader.i32()?,
               ids: reader.ints()?,
            }
         },
         4 => {
            Op::Delete {
               ids: reader.ints()?,
            }
         },
         5 => Op::Root { id: reader.i32()? },
         6 => {
            Op::Focus {
               id: if reader.u8()? == 0 {
                  None
               } else {
                  Some(reader.i32()?)
               },
            }
         },
         7 => {
            Op::ScrollTo {
               id:    reader.i32()?,
               value: reader.value(0)?,
            }
         },
         8 => Op::RequestFrame,
         9 => {
            Op::Motion {
               words: reader.ints()?,
            }
         },
         10 => {
            let id = reader.i32()?;
            let edits = (0..reader.count()?)
               .map(|_| {
                  Ok(ChildEdit {
                     keep: reader.i32()?,
                     del:  reader.i32()?,
                     ins:  reader.ints()?,
                  })
               })
               .collect::<Result<_, DecodeError>>()?;

            Op::ChildEdits { id, edits }
         },
         op => return Err(DecodeError::UnknownOp { at, op }),
      };
      ops.push(op);
   }
   Ok(ops)
}

fn put_i32_len(out: &mut Vec<u8>, len: usize) {
   let wire = i32::try_from(len).expect("length exceeds i32::MAX");
   out.extend_from_slice(&wire.to_le_bytes());
}

fn put_u16_len(out: &mut Vec<u8>, len: usize) {
   let wire = u16::try_from(len).expect("length exceeds u16::MAX");
   out.extend_from_slice(&wire.to_le_bytes());
}

fn put_str(out: &mut Vec<u8>, text: &str) {
   put_i32_len(out, text.len());
   out.extend_from_slice(text.as_bytes());
}

fn put_ints(out: &mut Vec<u8>, ints: &[i32]) {
   put_i32_len(out, ints.len());
   for int in ints {
      out.extend_from_slice(&int.to_le_bytes());
   }
}

fn put_entries(out: &mut Vec<u8>, entries: &[(Prop, Value)]) {
   for (prop, value) in entries {
      out.extend_from_slice(&prop.0.to_le_bytes());
      put_value(out, value);
   }
}

fn put_props(out: &mut Vec<u8>, props: &[(Prop, Value)]) {
   put_u16_len(out, props.len());
   put_entries(out, props);
}

fn put_value(out: &mut Vec<u8>, value: &Value) {
   match value {
      Value::Null => out.push(0),
      Value::Bool(false) => out.push(1),
      Value::Bool(true) => out.push(2),
      Value::I32(int) => {
         out.push(3);
         out.extend_from_slice(&int.to_le_bytes());
      },
      Value::I64(int) => {
         out.push(4);
         out.extend_from_slice(&int.to_le_bytes());
      },
      Value::F64(float) => {
         out.push(5);
         out.extend_from_slice(&float.to_le_bytes());
      },
      Value::Str(text) => {
         out.push(6);
         put_str(out, text);
      },
      Value::Arr(items) => {
         out.push(7);
         put_i32_len(out, items.len());
         for item in items {
            put_value(out, item);
         }
      },
      Value::Obj(entries) => {
         out.push(8);
         put_i32_len(out, entries.len());
         put_entries(out, entries);
      },
      Value::Rect(rect) => {
         out.push(9);
         for side in rect {
            out.extend_from_slice(&side.to_le_bytes());
         }
      },
      Value::Map(pairs) => {
         out.push(10);
         put_i32_len(out, pairs.len());
         for (key, item) in pairs {
            put_str(out, key);
            put_value(out, item);
         }
      },
      Value::Words(words) => {
         out.push(11);
         put_ints(out, words);
      },
      Value::WPatch { list, set } => {
         out.push(12);
         out.extend_from_slice(&list.to_le_bytes());
         put_u16_len(out, set.len());
         for (idx, word) in set {
            out.extend_from_slice(&idx.to_le_bytes());
            out.extend_from_slice(&word.to_le_bytes());
         }
      },
   }
}

#[must_use]
pub fn encode(ops: &[Op]) -> Vec<u8> {
   let mut out = vec![VERSION];
   for op in ops {
      match op {
         Op::Create { id, kind, props } => {
            out.push(1);
            out.extend_from_slice(&id.to_le_bytes());
            put_u16_len(&mut out, kind.len());
            out.extend_from_slice(kind.as_bytes());
            put_props(&mut out, props);
         },
         Op::Update { id, props } => {
            out.push(2);
            out.extend_from_slice(&id.to_le_bytes());
            put_props(&mut out, props);
         },
         Op::Children { id, ids } => {
            out.push(3);
            out.extend_from_slice(&id.to_le_bytes());
            put_ints(&mut out, ids);
         },
         Op::Delete { ids } => {
            out.push(4);
            put_ints(&mut out, ids);
         },
         Op::Root { id } => {
            out.push(5);
            out.extend_from_slice(&id.to_le_bytes());
         },
         Op::Focus { id: None } => out.extend_from_slice(&[6, 0]),
         Op::Focus { id: Some(id) } => {
            out.extend_from_slice(&[6, 1]);
            out.extend_from_slice(&id.to_le_bytes());
         },
         Op::ScrollTo { id, value } => {
            out.push(7);
            out.extend_from_slice(&id.to_le_bytes());
            put_value(&mut out, value);
         },
         Op::RequestFrame => out.push(8),
         Op::Motion { words } => {
            out.push(9);
            put_ints(&mut out, words);
         },
         Op::ChildEdits { id, edits } => {
            out.push(10);
            out.extend_from_slice(&id.to_le_bytes());
            put_i32_len(&mut out, edits.len());
            for edit in edits {
               out.extend_from_slice(&edit.keep.to_le_bytes());
               out.extend_from_slice(&edit.del.to_le_bytes());
               put_ints(&mut out, &edit.ins);
            }
         },
      }
   }
   out
}

/// One frame of a dispatch call. `offset` and `len` are its byte range in the
/// input.
#[derive(Clone, Debug, PartialEq)]
pub struct Dispatch {
   pub offset: usize,
   pub len:    usize,
   pub event:  DispatchEvent,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DispatchEvent {
   View {
      node:    i32,
      event:   Prop,
      payload: Value,
   },
   Other,
}

const BATCH_HEADER: usize = 16;

fn u32_at(bytes: &[u8], pos: usize) -> Option<usize> {
   let chunk = bytes.get(pos..)?.first_chunk::<4>()?;
   usize::try_from(u32::from_le_bytes(*chunk)).ok()
}

/// Splits a `nativeDispatch` call into frames.
///
/// A kind-4 batch (`f7.t.d`) is `04 00 u16 0 u16 0 u16 0 u32 0 u32 hdrLen
/// hdr[hdrLen]` then `{u32 len, frame[len]}*`. A view frame (`yq.d`) is
/// `00 00 00 00 01 01 i32 node u16 event` and an optional tagged payload.
pub fn decode_dispatch(bytes: &[u8]) -> Result<Vec<Dispatch>, DecodeError> {
   let mut out = Vec::new();
   walk(bytes, 0, bytes.len(), 0, &mut out)?;
   Ok(out)
}

fn walk(
   bytes: &[u8],
   offset: usize,
   len: usize,
   depth: usize,
   out: &mut Vec<Dispatch>,
) -> Result<(), DecodeError> {
   if depth > MAX_DEPTH {
      return Err(DecodeError::TooDeep { at: offset });
   }

   let end = offset + len;
   let frame = &bytes[offset..end];
   match frame {
      [4, ..] => {
         let header = u32_at(frame, 12).ok_or(DecodeError::Underflow { at: offset })?;
         let mut pos = BATCH_HEADER.saturating_add(header);
         while pos.saturating_add(4) <= len {
            let inner = u32_at(frame, pos).ok_or(DecodeError::Underflow { at: offset + pos })?;
            let start = pos + 4;
            let stop = start
               .checked_add(inner)
               .filter(|&stop| stop <= len)
               .ok_or(DecodeError::FrameOverrun { at: offset + pos })?;

            walk(bytes, offset + start, inner, depth + 1, out)?;
            pos = stop;
         }
      },
      [0, _, _, _, 1, 1, ..] if len >= 12 => {
         let mut reader = Reader {
            bytes: &bytes[..end],
            pos:   offset + 6,
         };
         let node = reader.i32()?;
         let event = Prop(reader.u16()?);
         let payload = if reader.at_end() {
            Value::Null
         } else {
            reader.value(0)?
         };
         out.push(Dispatch {
            offset,
            len,
            event: DispatchEvent::View {
               node,
               event,
               payload,
            },
         });
      },
      _ => {
         out.push(Dispatch {
            offset,
            len,
            event: DispatchEvent::Other,
         });
      },
   }
   Ok(())
}

/// Rebuilds a kind-4 batch without the top-level frames at `offsets`. Any other
/// input is one frame at offset 0, so it comes back empty when `offsets` holds
/// 0 and unchanged otherwise.
#[must_use]
pub fn without_frames(bytes: &[u8], offsets: &[usize]) -> Vec<u8> {
   if bytes.first() != Some(&4) {
      return if offsets.contains(&0) {
         Vec::new()
      } else {
         bytes.to_vec()
      };
   }

   let Some(header) = u32_at(bytes, 12) else {
      return bytes.to_vec();
   };

   let mut pos = BATCH_HEADER.saturating_add(header);
   let mut out = bytes[..pos.min(bytes.len())].to_vec();
   while let Some(inner) = u32_at(bytes, pos) {
      let stop = (pos + 4).saturating_add(inner);
      if !offsets.contains(&(pos + 4)) {
         out.extend_from_slice(&bytes[pos..stop.min(bytes.len())]);
      }
      pos = stop;
   }
   out
}

#[derive(Clone, Debug, PartialEq)]
pub struct Node {
   pub kind:  String,
   pub props: Props,
   pub kids:  Vec<Self>,
   pub src:   Option<u32>,
}

/// Phoenix's own settings layouts, captured from versionCode 356207.
#[derive(Debug)]
pub struct Templates {
   pub section:    Node,
   pub header:     Node,
   pub group:      Node,
   pub switch_row: Node,
   pub divider:    Node,
   pub button_row: Node,
   pub value_row:  Node,
   pub input:      Node,
   pub page:       Node,
   pub track_off:  Props,
   pub knob_off:   Props,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawTemplates {
   section:    RawNode,
   header:     RawNode,
   group:      RawNode,
   switch_row: RawNode,
   divider:    RawNode,
   button_row: RawNode,
   value_row:  RawNode,
   input:      RawNode,
   page:       RawNode,
   track_off:  Vec<RawEntry>,
   knob_off:   Vec<RawEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawNode {
   #[serde(rename = "type")]
   kind:  String,
   props: Vec<RawEntry>,
   kids:  Vec<Self>,
   src:   Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEntry {
   k: String,
   v: RawValue,
}

#[derive(Deserialize)]
struct RawPatch {
   list: i32,
   set:  Vec<(u16, i32)>,
}

#[derive(Deserialize)]
#[serde(tag = "t", content = "v", rename_all = "lowercase")]
enum RawValue {
   Null,
   False,
   True,
   I32(i32),
   I64(String),
   F64(f64),
   Str(String),
   Arr(Vec<Self>),
   Obj(Vec<RawEntry>),
   Rect([f32; 4]),
   Map(Vec<RawEntry>),
   Words(Vec<i32>),
   Wpatch(RawPatch),
}

impl From<RawValue> for Value {
   fn from(raw: RawValue) -> Self {
      match raw {
         RawValue::Null => Self::Null,
         RawValue::False => Self::Bool(false),
         RawValue::True => Self::Bool(true),
         RawValue::I32(int) => Self::I32(int),
         RawValue::I64(text) => {
            Self::I64(
               text
                  .parse()
                  .unwrap_or_else(|err| panic!("templates.json: i64 {text:?}: {err}")),
            )
         },
         RawValue::F64(float) => Self::F64(float),
         RawValue::Str(text) => Self::Str(text),
         RawValue::Arr(items) => Self::Arr(items.into_iter().map(Self::from).collect()),
         RawValue::Obj(entries) => Self::Obj(props(entries)),
         RawValue::Rect(rect) => Self::Rect(rect),
         RawValue::Map(entries) => {
            Self::Map(
               entries
                  .into_iter()
                  .map(|entry| (entry.k, entry.v.into()))
                  .collect(),
            )
         },
         RawValue::Words(words) => Self::Words(words),
         RawValue::Wpatch(patch) => {
            Self::WPatch {
               list: patch.list,
               set:  patch.set,
            }
         },
      }
   }
}

fn props(entries: Vec<RawEntry>) -> Props {
   entries
      .into_iter()
      .map(|entry| {
         let prop = entry
            .k
            .parse()
            .unwrap_or_else(|err| panic!("templates.json: {err}"));
         (prop, entry.v.into())
      })
      .collect()
}

impl From<RawNode> for Node {
   fn from(raw: RawNode) -> Self {
      Self {
         kind:  raw.kind,
         props: props(raw.props),
         kids:  raw.kids.into_iter().map(Self::from).collect(),
         src:   raw.src,
      }
   }
}

pub fn templates() -> &'static Templates {
   static TEMPLATES: OnceLock<Templates> = OnceLock::new();
   TEMPLATES.get_or_init(|| {
      let raw: RawTemplates = serde_json::from_str(include_str!("templates.json"))
         .unwrap_or_else(|err| panic!("templates.json: {err}"));

      Templates {
         section:    raw.section.into(),
         header:     raw.header.into(),
         group:      raw.group.into(),
         switch_row: raw.switch_row.into(),
         divider:    raw.divider.into(),
         button_row: raw.button_row.into(),
         value_row:  raw.value_row.into(),
         input:      raw.input.into(),
         page:       raw.page.into(),
         track_off:  props(raw.track_off),
         knob_off:   props(raw.knob_off),
      }
   })
}
