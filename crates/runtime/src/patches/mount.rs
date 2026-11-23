//! `phoenix.ui.mount`: raw plugin-built subtrees under any Phoenix node.

use phoenix_ui::Op;

use crate::patches::{
   Before,
   Draft,
   Handler,
   MountBefore,
   MountNode,
   Patches,
   Place,
   Slot,
   SlotKind,
   patches,
};

impl Patches {
   pub fn open_mount(
      &mut self,
      id: u32,
      parent: Option<i32>,
      before: MountBefore,
      node: MountNode,
   ) -> Vec<Op> {
      let Some(parent) = parent.or_else(|| self.mirror.root()) else {
         log::warn!("mount {id}: parent/root not available");
         return Vec::new();
      };
      if !self.mirror.has_kids(parent) && !self.mirror.exists(parent) {
         log::warn!("mount {id}: parent {parent} not found in mirror");
         return Vec::new();
      }
      let draft = to_draft(id, node);
      let before = match before {
         MountBefore::Start => Before::Start,
         MountBefore::End => Before::End,
         MountBefore::Node(sib) => Before::Node(sib),
      };
      let mut out = Vec::new();
      let mut all = Vec::new();
      let root = self.emit(draft, None, &mut out, &mut all);
      self.slots.push(Slot {
         kind: SlotKind::Mounted(id),
         anchor: parent,
         parent,
         before,
         place: Place::Screen,
         roots: vec![root],
         all,
      });
      out.push(Op::Children {
         id:  parent,
         ids: self.merged(parent),
      });
      out
   }

   fn close_mount(&mut self, id: u32) -> Vec<Op> {
      let Some(at) = self
         .slots
         .iter()
         .position(|slot| slot.kind == SlotKind::Mounted(id))
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

fn to_draft(mount: u32, node: MountNode) -> Draft {
   let press = (node.on_press.is_some() || node.on_dismiss.is_some()).then_some(Handler::Mounted {
      mount,
      press: node.on_press,
      dismiss: node.on_dismiss,
   });
   Draft {
      kind: node.kind,
      props: node.props,
      kids: node
         .kids
         .into_iter()
         .map(|kid| to_draft(mount, kid))
         .collect(),
      press,
      src: None,
   }
}

pub fn unmount(id: u32) -> Vec<Op> {
   let ops = patches().close_mount(id);
   if let Some(engine) = crate::engine() {
      engine.mount_closed(id);
   }
   ops
}
