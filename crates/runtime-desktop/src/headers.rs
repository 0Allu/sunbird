//! Plugin icons after the author's name in message headers
//! (`phoenix.ui.addMessageHeaderDecoration`) on Desktop Phoenix.
//!
//! Headers are Discord's own `Rc<Node>` trees, not GPUI elements. Each display
//! mode's header row adds the name wrapper with one `child_node` call, found by
//! [`desktop::message_header`]; those calls are pointed at [`name_detour`],
//! which adds the decorations' icons as Discord's node SVG leaves right after.
//!
//! The header only knows the message id. `MessageRow::render`, hooked in
//! [`crate::accessories`], hands the row it renders through [`enter`], and the
//! row holds the message with its author.

use std::{
   cell::Cell,
   ffi::c_void,
   sync::{
      OnceLock,
      atomic::{
         AtomicBool,
         Ordering,
      },
   },
};

use phoenix_hook::image::Image;
use phoenix_sigs::desktop;

use crate::{
   accessories,
   members::{
      HEADERS,
      MIN_SNOWFLAKE,
   },
};

/// `child_node(node, child) -> node`, `Rc<Node>` handles in and out.
type ChildNodeFn = unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void;
/// `svg_new(out, path ptr, path len, size)`, writing a node SVG leaf.
type SvgNewFn = unsafe extern "C" fn(*mut u8, *const u8, usize, f32) -> *mut u8;
/// `push_svg(node, &leaf) -> node`, moving the leaf into a child.
type PushSvgFn = unsafe extern "C" fn(*mut c_void, *mut u8) -> *mut c_void;

/// Room for the leaf, which is 0x38 bytes on builds so far; a larger one turns
/// the feature off.
const LEAF_WORDS: usize = 32;
/// The name's line height in Discord's headers, in px.
const ICON_SIZE: f32 = 16.0;
/// One detour per header builder.
const SITES: usize = 2;

struct Symbols {
   child_node: ChildNodeFn,
   svg_new:    SvgNewFn,
   push_svg:   PushSvgFn,
   svg_color:  usize,
   /// `[rbp+slot]` holds the message id at each site.
   slots:      [usize; SITES],
   /// `MessageRow` keeps the author's `Arc<User>` here.
   row_author: usize,
   /// The `User` in the `Arc`, and its id in the `User`.
   user_id:    usize,
}

static SYMBOLS: OnceLock<Symbols> = OnceLock::new();
static READY: AtomicBool = AtomicBool::new(false);

thread_local! {
   /// The message id and `MessageRow` being rendered on this thread.
   static ROW: Cell<(u64, usize)> = const { Cell::new((0, 0)) };
}

/// Marks `row` as rendering on this thread, returning what it replaces for
/// [`leave`].
pub fn enter(row: *const u8, message_id: u64) -> (u64, usize) {
   if !READY.load(Ordering::Relaxed) {
      return (0, 0);
   }
   ROW.try_with(|cell| cell.replace((message_id, row as usize)))
      .unwrap_or_default()
}

pub fn leave(outer: (u64, usize)) {
   if READY.load(Ordering::Relaxed) {
      let _ = ROW.try_with(|cell| cell.set(outer));
   }
}

/// Locates the header builders and hooks their name calls, from `DllMain`,
/// after [`accessories::install`].
pub fn install(image: &Image<'static>) {
   if !accessories::row_hooked() {
      log::warn!("desktop headers: MessageRow is not hooked, skipping");
      return;
   }
   let (symbols, sites) = match locate(image) {
      Ok(found) => found,
      Err(err) => {
         log::warn!("desktop headers: {err}, skipping");
         return;
      },
   };
   let detours: [usize; SITES] = [
      name_detour::<0> as *const () as usize,
      name_detour::<1> as *const () as usize,
   ];
   let child_node = symbols.child_node as usize;
   if SYMBOLS.set(symbols).is_err() {
      return;
   }
   for (site, detour) in sites.into_iter().zip(detours) {
      // SAFETY: runs from `prepare` before Discord threads execute; the island
      // only sets `r9`, which the two-argument `child_node` does not read, and
      // the detour calls `child_node` itself, stored above.
      match unsafe {
         image.hook_call(
            site,
            &desktop::mov_r9_from(phoenix_hook::x86::Register::RBP),
            detour,
         )
      } {
         Ok(target) if target == child_node => {
            READY.store(true, Ordering::Release);
            log::info!("desktop headers: hooked the header name @ {site:#x}");
         },
         Ok(target) => log::warn!("desktop headers: {site:#x} called {target:#x}"),
         Err(err) => log::warn!("desktop headers: header name hook failed: {err}"),
      }
   }
}

/// The symbols, and the call adding the name in each header builder.
fn locate(image: &Image<'_>) -> Result<(Symbols, [usize; SITES]), String> {
   let row = desktop::message_row(image)?;
   let body = desktop::message_body(image, row.func)?;
   let header = desktop::message_header(image, body.func)?;
   let json = desktop::message_json(image)?;
   let author = desktop::message_author(image, json.to_value)?;
   let row_author = desktop::row_author(image, &row, &author)?;
   let [first, second] = header.sites.as_slice() else {
      return Err(format!("{} header name calls", header.sites.len()));
   };
   let svg = header.svg;
   if svg.size > LEAF_WORDS * 8 {
      return Err(format!("the node SVG leaf is {:#x} bytes", svg.size));
   }
   // SAFETY: decoded from Discord's own calls with these signatures: two
   // handles in `rcx`/`rdx`; `out`, path and size in `rcx`, `rdx`, `r8`,
   // `xmm3`; the handle and the leaf's address.
   let symbols = unsafe {
      Symbols {
         child_node: std::mem::transmute::<usize, ChildNodeFn>(header.child_node),
         svg_new: std::mem::transmute::<usize, SvgNewFn>(svg.new),
         push_svg: std::mem::transmute::<usize, PushSvgFn>(svg.push),
         svg_color: svg.color,
         slots: [first.slot, second.slot],
         row_author,
         user_id: author.arc_data + author.user_id,
      }
   };
   Ok((symbols, [first.site, second.site]))
}

/// The author of the message `frame`'s header is for, when it is the row this
/// thread is rendering.
///
/// # Safety
///
/// `frame` is the header builder's `rbp` at a decoded site.
unsafe fn author(symbols: &Symbols, site: usize, frame: *const u8) -> Option<u64> {
   let (current, row) = ROW.try_with(Cell::get).ok()?;
   let slot = *symbols.slots.get(site)?;
   // SAFETY: the builder keeps the message id at `[rbp+slot]` at the call.
   let message_id = unsafe { frame.add(slot).cast::<u64>().read_unaligned() };
   if row == 0 || message_id != current || message_id < MIN_SNOWFLAKE {
      return None;
   }
   // SAFETY: `row` is the live `MessageRow` whose render called this header,
   // which keeps an `Arc<User>` at the decoded offset.
   let user = unsafe {
      (row as *const u8)
         .add(symbols.row_author)
         .cast::<usize>()
         .read_unaligned()
   };
   if user == 0 || user % 8 != 0 {
      return None;
   }
   // SAFETY: a live `Arc<User>`, its id at the decoded offset.
   let id = unsafe {
      (user as *const u8)
         .add(symbols.user_id)
         .cast::<u64>()
         .read_unaligned()
   };
   (id >= MIN_SNOWFLAKE).then_some(id)
}

/// The icons the header decorations drew for the author at `frame`, queueing
/// them when unknown.
fn icons(symbols: &Symbols, site: usize, frame: *const u8) -> Vec<(&'static str, Option<u32>)> {
   // SAFETY: forwarded from the detour, whose `frame` is the builder's `rbp`.
   let Some(user_id) = (unsafe { author(symbols, site, frame) }) else {
      return Vec::new();
   };
   HEADERS
      .lookup(user_id)
      .unwrap_or_default()
      .into_iter()
      .flat_map(|item| item.icons)
      .collect()
}

/// `child_node` adding the name wrapper, then the decorations' icons. The
/// island hands it the builder's `rbp` as `frame`.
unsafe extern "C" fn name_detour<const SITE: usize>(
   node: *mut c_void,
   child: *mut c_void,
   _r8: usize,
   frame: *const u8,
) -> *mut c_void {
   let Some(symbols) = SYMBOLS.get() else {
      return node;
   };
   // SAFETY: forwards `child_node`'s own arguments.
   let mut node = unsafe { (symbols.child_node)(node, child) };
   if !HEADERS.active() || node.is_null() || frame.is_null() {
      return node;
   }
   let Ok(icons) = std::panic::catch_unwind(|| icons(symbols, SITE, frame)) else {
      return node;
   };
   for (path, color) in icons {
      let mut leaf = [0u64; LEAF_WORDS];
      let leaf = leaf.as_mut_ptr().cast::<u8>();
      // SAFETY: `leaf` has room for the node SVG leaf; Discord copies `path`
      // into its own string, and `push_svg` moves the leaf into the node and
      // returns the node's handle, which may be a fresh clone.
      unsafe {
         (symbols.svg_new)(leaf, path.as_ptr(), path.len(), ICON_SIZE);
         if let Some(argb) = color {
            leaf.add(symbols.svg_color).cast::<u32>().write_unaligned(1);
            leaf
               .add(symbols.svg_color + 4)
               .cast::<u32>()
               .write_unaligned(accessories::argb_to_rgba(argb));
         }
         node = (symbols.push_svg)(node, leaf);
      }
      if node.is_null() {
         break;
      }
   }
   node
}
