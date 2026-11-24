//! Plugin menu sheets, like the message long-press one, and their answers.

use std::sync::{
   atomic::{
      AtomicU64,
      Ordering,
   },
   mpsc::{
      Sender,
      channel,
   },
};

use phoenix_ui_mobile::Op;

use crate::patches::{
   Before,
   Handler,
   Patches,
   Place,
   Slot,
   SlotKind,
   targets,
   worker::{
      Job,
      queue,
   },
};

/// A menu sheet row a plugin asked for.
pub struct MenuItem {
   pub label:  String,
   pub icon:   Option<String>,
   pub danger: bool,
}

/// Scrollable text content shown inside a plugin sheet.
pub struct SheetBody {
   pub text:      String,
   pub monospace: bool,
   pub highlight: bool,
}

/// Shows a menu sheet like the message long-press one and waits for the
/// choice: the row's index, -1 when it was swiped away, or `None` when no
/// such sheet can be drawn yet.
pub fn menu(title: Option<String>, items: Vec<MenuItem>) -> Option<i32> {
   let (answer, receiver) = channel();
   queue(Job::Menu {
      title,
      body: None,
      items,
      answer,
   });
   match receiver.recv() {
      Ok(-2) | Err(_) => None,
      Ok(index) => Some(index),
   }
}

/// Shows a scrollable text/code viewer sheet with optional action rows.
pub fn sheet(title: Option<String>, body: SheetBody, items: Vec<MenuItem>) -> Option<i32> {
   let (answer, receiver) = channel();
   queue(Job::Menu {
      title,
      body: Some(body),
      items,
      answer,
   });
   match receiver.recv() {
      Ok(-2) | Err(_) => None,
      Ok(index) => Some(index),
   }
}

impl Patches {
   /// Shows a menu sheet over the top sheet, like the message long-press one.
   pub fn open_menu(
      &mut self,
      title: Option<&str>,
      body: Option<&SheetBody>,
      items: Vec<MenuItem>,
      answer: Sender<i32>,
   ) -> Vec<Op> {
      static NEXT: AtomicU64 = AtomicU64::new(1);
      #[expect(clippy::cast_possible_truncation, reason = "menus are few")]
      let menu = NEXT.fetch_add(1, Ordering::Relaxed) as u32;
      let host = self
         .mirror
         .newest_sheet()
         .and_then(|sheet| self.mirror.parent(sheet))
         .and_then(|outer| self.mirror.parent(outer))
         .or_else(|| self.sheet_host.filter(|host| self.mirror.has_kids(*host)));
      let rows = items
         .into_iter()
         .enumerate()
         .map(|(index, item)| {
            targets::MenuRow {
               label:  item.label,
               icon:   item.icon,
               danger: item.danger,
               press:  Handler::Choice {
                  menu,
                  index: i32::try_from(index).unwrap_or_default(),
               },
               id:     None,
            }
         })
         .collect();
      let draft = self
         .templates
         .menu(title, body, rows, &Handler::Choice { menu, index: -1 });
      let (Some(host), Some(draft)) = (host, draft) else {
         let _ = answer.send(-2);
         return Vec::new();
      };
      let mut out = Vec::new();
      let mut all = Vec::new();
      let root = self.emit(draft, None, &mut out, &mut all);
      self.slots.push(Slot {
         kind: SlotKind::Menu(menu),
         anchor: host,
         parent: host,
         before: Before::End,
         place: Place::Screen,
         roots: vec![root],
         all,
      });
      self.menus.insert(menu, answer);
      out.push(Op::Children {
         id:  host,
         ids: self.merged(host),
      });
      out
   }

   pub fn close_menu(&mut self, menu: u32, index: i32) -> Vec<Op> {
      if let Some(answer) = self.menus.remove(&menu) {
         let _ = answer.send(index);
      }
      let Some(at) = self
         .slots
         .iter()
         .position(|slot| slot.kind == SlotKind::Menu(menu))
      else {
         return Vec::new();
      };
      let slot = self.slots.remove(at);
      self.forget_ids(&slot.all);
      let mut out = Vec::new();
      if self.mirror.has_kids(slot.parent) {
         out.push(Op::Children {
            id:  slot.parent,
            ids: self.merged(slot.parent),
         });
      }
      out.push(Op::Delete { ids: slot.all });
      out
   }
}
