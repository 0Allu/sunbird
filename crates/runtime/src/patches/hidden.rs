//! `phoenix.ui.hide`: keeps `hidden: true` on the nodes plugins hide.

use std::collections::HashSet;

use phoenix_ui::Op;
use phoenix_wire::Value;

use crate::patches::{
   HIDDEN,
   Patches,
};

impl Patches {
   /// Walks up through unnamed single-child wrapper views so hiding a button
   /// collapses its fixed-size slot wrapper too (no empty gap).
   fn hide_root(&self, mut id: i32) -> i32 {
      for _ in 0..4 {
         let Some(parent) = self.mirror.parent(id) else {
            break;
         };
         if self.mirror.accessibility_id(parent).is_some() || self.mirror.kids(parent).len() != 1 {
            break;
         }
         id = parent;
      }
      id
   }

   /// Reconciles `hidden: true` (`View.GONE`) across all live wrappers matching
   /// `self.hidden`, restoring `hidden: false` whenever a node stops matching
   /// (e.g. `"Record Voice Message"` swapping to `"send"` inside the same
   /// slot).
   pub fn reconcile_hidden(&mut self) -> Vec<Op> {
      if self.hidden.is_empty() && self.hidden_nodes.is_empty() {
         return Vec::new();
      }
      let mut desired = HashSet::new();
      for aid in self.hidden.keys() {
         for id in self.mirror.nodes_with_id(aid) {
            desired.insert(self.hide_root(id));
         }
      }
      let mut ops = Vec::new();
      for &id in &desired {
         if !self.hidden_nodes.contains(&id) {
            ops.push(Op::Update {
               id,
               props: vec![(HIDDEN, Value::Bool(true))],
            });
         }
      }
      for &id in &self.hidden_nodes {
         if !desired.contains(&id) && self.mirror.exists(id) {
            ops.push(Op::Update {
               id,
               props: vec![(HIDDEN, Value::Bool(false))],
            });
         }
      }
      self.hidden_nodes = desired;
      ops
   }
}
