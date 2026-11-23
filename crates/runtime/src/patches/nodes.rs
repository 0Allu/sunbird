//! Raw node access for `phoenix.ui`: node snapshots and `onNode` watchers.

use crate::patches::{
   NodeInfo,
   Patches,
   worker::{
      Job,
      queue,
   },
};

impl Patches {
   pub fn notify_watchers(&self, named: &[i32]) {
      // Notify raw node watchers (`phoenix.ui.onNode`) for matching newly
      // created nodes.
      if !self.node_watchers.is_empty() {
         for id in named {
            let Some(aid) = self.mirror.accessibility_id(*id) else {
               continue;
            };
            let watchers = self
               .node_watchers
               .iter()
               .filter(|watcher| watcher.matcher.matches(aid))
               .map(|watcher| watcher.id)
               .collect::<Vec<_>>();
            if !watchers.is_empty() {
               queue(Job::NodeCreated {
                  watchers,
                  info: self.build_node_info(*id),
               });
            }
         }
      }
   }

   pub fn build_node_info(&self, id: i32) -> NodeInfo {
      let on = self
         .mirror
         .list_on(id)
         .iter()
         .filter_map(|v| v.as_str().map(str::to_owned))
         .collect();
      NodeInfo {
         id,
         kind: self.mirror.kind(id).map(|k| k.as_str().to_owned()),
         accessibility_id: self.mirror.accessibility_id(id).map(str::to_owned),
         parent: self.mirror.parent(id),
         children: self.mirror.kids(id).to_vec(),
         sheet: self.mirror.sheet_of(id),
         list: self.mirror.enclosing_list(id),
         on,
      }
   }
}
