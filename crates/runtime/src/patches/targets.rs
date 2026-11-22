use phoenix_ui::{
   Node,
   Prop,
   Props,
   ViewKind,
};
use phoenix_wire::Value;

use crate::patches::{
   Contribution,
   Draft,
   Handler,
   Outcome,
   Place,
   Rendered,
   mirror::{
      Created,
      Mirror,
   },
};

const DANGER: i64 = 4_294_475_386;
const MUTED: i64 = 4_288_059_294;
const DEFAULT_ICON: &str = "res://ic_mana_puzzle_piece_plus";
const ROW_TEMPLATES: [&str; 6] = [
   "message.actions.reply",
   "message.actions.copy_text",
   "message.actions.copy_id",
   "message.actions.mark_unread",
   "message.actions.copy_link",
   "message.actions.mention",
];

/// Where a contribution goes, next to one of Phoenix's own nodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Target {
   /// The chat header, left of search.
   Header,
   /// The message box, left of the emoji button.
   Composer,
   /// Under a message's content.
   Content,
   /// After a message's timestamp.
   Timestamp,
   /// The message long-press menu, above Delete.
   Action,
   /// The end of a profile's main tab.
   Profile,
}

impl Target {
   pub const ALL: [Self; 6] = [
      Self::Header,
      Self::Composer,
      Self::Content,
      Self::Timestamp,
      Self::Action,
      Self::Profile,
   ];

   pub fn matches(self, aid: &str) -> bool {
      match self {
         Self::Header => aid == "search.entry",
         Self::Composer => aid == "composer.expressions",
         Self::Content => aid.starts_with("message.content:"),
         Self::Timestamp => aid.starts_with("message.timestamp:"),
         Self::Action => aid == "message.actions.root",
         Self::Profile => aid == "profile-tab-pages",
      }
   }

   /// Where this target's nodes go for the anchor `id`.
   pub fn place(self, mirror: &Mirror, id: i32, aid: &str, seen: &Seen) -> Option<Placement> {
      match self {
         Self::Header | Self::Composer => {
            Some(Placement {
               parent: mirror.parent(id)?,
               before: Some(id),
               place:  Place::Screen,
            })
         },
         Self::Content => {
            Some(Placement {
               parent: id,
               before: None,
               place:  Place::Message(aid.strip_prefix("message.content:")?.to_owned()),
            })
         },
         Self::Timestamp => {
            Some(Placement {
               parent: mirror.parent(id)?,
               before: None,
               place:  Place::Message(aid.strip_prefix("message.timestamp:")?.to_owned()),
            })
         },
         Self::Action => {
            let groups = *mirror.kids(id).last()?;
            let delete = mirror.kids(groups).iter().copied().find(|group| {
               mirror
                  .find_down(*group, &|aid| aid == "message.actions.delete", 2)
                  .is_some()
            });
            Some(Placement {
               parent: groups,
               before: delete,
               place:  seen
                  .long_pressed
                  .clone()
                  .map_or(Place::Screen, Place::Message),
            })
         },
         Self::Profile => {
            // Phoenix nests the sections six levels under the pages row.
            let mut node = id;
            for _ in 0..6 {
               node = *mirror.kids(node).first()?;
            }
            Some(Placement {
               parent: node,
               before: None,
               place:  Place::User(seen.profile_user.clone()),
            })
         },
      }
   }

   /// Whether drawing this target needs a plugin's `render` or `visible`.
   pub const fn evaluates(self) -> bool {
      matches!(self, Self::Content | Self::Timestamp | Self::Profile)
   }
}

/// What the dispatches and batches showed about the screen.
#[derive(Default)]
pub struct Seen {
   pub long_pressed: Option<String>,
   pub profile_user: Option<String>,
}

pub struct Placement {
   pub parent: i32,
   /// The sibling the nodes go before, or the end.
   pub before: Option<i32>,
   pub place:  Place,
}

/// Phoenix's own nodes, snapshotted when they appear, so plugin nodes look
/// like their neighbours.
#[derive(Default)]
pub struct Templates {
   header:    Option<Node>,
   composer:  Option<Node>,
   timestamp: Option<Node>,
   row:       Option<Node>,
   group:     Option<(ViewKind, Props)>,
   divider:   Option<Node>,
   profile:   Option<Node>,
}

impl Templates {
   pub fn learn(&mut self, mirror: &Mirror, id: i32, aid: &str, created: &Created<'_>) {
      match aid {
         "search.entry" => self.header = mirror.snapshot(id, created),
         "composer.expressions" => self.composer = mirror.snapshot(id, created),
         "profile-tab-pages" => self.learn_profile(mirror, id, created),
         _ if aid.starts_with("message.timestamp:") && self.timestamp.is_none() => {
            self.timestamp = mirror.snapshot(id, created);
         },
         _ if ROW_TEMPLATES.contains(&aid) => self.learn_row(mirror, id, created),
         _ => {},
      }
   }

   fn learn_row(&mut self, mirror: &Mirror, id: i32, created: &Created<'_>) {
      let Some(row) = mirror.snapshot(id, created) else {
         return;
      };
      if row.kids.first().is_none_or(|inner| inner.kids.len() < 2) {
         return;
      }
      let group = mirror.parent(id);
      let siblings = group.map_or(&[][..], |group| mirror.kids(group));
      let next = siblings
         .iter()
         .position(|sibling| *sibling == id)
         .and_then(|at| siblings.get(at + 1))
         .copied();
      if let Some(divider) = next
         .filter(|next| mirror.accessibility_id(*next).is_none())
         .and_then(|next| mirror.snapshot(next, created))
      {
         self.divider = Some(divider);
      }
      if let Some((kind, props)) = group.and_then(|group| created.get(&group)) {
         self.group = Some(((*kind).clone(), (*props).clone()));
      }
      self.row = Some(row);
   }

   fn learn_profile(&mut self, mirror: &Mirror, id: i32, created: &Created<'_>) {
      let seen = Seen::default();
      let Some(placement) = Target::Profile.place(mirror, id, "", &seen) else {
         return;
      };
      let first = mirror
         .kids(placement.parent)
         .first()
         .and_then(|section| mirror.kids(*section).first());
      let Some(section) = first.and_then(|first| mirror.snapshot(*first, created)) else {
         return;
      };
      let shaped = section
         .kids
         .first()
         .is_some_and(|head| head.kind == ViewKind::Text)
         && section
            .kids
            .get(1)
            .and_then(|body| body.kids.first())
            .is_some_and(|text| text.kind == ViewKind::Text);
      if shaped {
         self.profile = Some(section);
      }
   }

   /// The nodes `contributions` draw at `place`. `outcome` answers what
   /// plugins already evaluated there, `None` while it is pending.
   pub fn draw(
      &self,
      target: Target,
      contributions: &[&Contribution],
      place: &Place,
      outcome: &impl Fn(&Contribution) -> Option<Outcome>,
   ) -> Vec<Draft> {
      match target {
         Target::Header => Self::icon_buttons(self.header.as_ref(), contributions, place),
         Target::Composer => Self::icon_buttons(self.composer.as_ref(), contributions, place),
         Target::Content | Target::Timestamp => {
            contributions
               .iter()
               .filter_map(|contribution| {
                  let Some(Outcome::Text(Some(rendered))) = outcome(contribution) else {
                     return None;
                  };
                  // A tint alone draws no line.
                  if rendered.text.is_empty() {
                     return None;
                  }
                  let mut node = text(&self.accessory_props(), &rendered);
                  if target == Target::Content {
                     node.set(Prop::MARGIN, Some(Value::Rect([0.0, 2.0, 0.0, 0.0])));
                  }
                  Some(node)
               })
               .collect()
         },
         Target::Action => self.action_group(contributions, place, outcome),
         Target::Profile => {
            let Some(section) = &self.profile else {
               return Vec::new();
            };
            contributions
               .iter()
               .filter_map(|contribution| {
                  let Some(Outcome::Text(Some(rendered))) = outcome(contribution) else {
                     return None;
                  };
                  let mut node = Draft::from_node(section);
                  let mut head = text(&section.kids[0].props, &Rendered {
                     text:  contribution.label.clone(),
                     color: None,
                     tint:  None,
                  });
                  head.set(
                     Prop::ACCESSIBILITY_LABEL,
                     Some(Value::Str(contribution.label.clone())),
                  );
                  let body = text(&section.kids[1].kids[0].props, &rendered);
                  node.kids.truncate(2);
                  node.kids[0] = head;
                  node.kids[1].kids = vec![body];
                  Some(node)
               })
               .collect()
         },
      }
   }

   fn icon_buttons(
      template: Option<&Node>,
      contributions: &[&Contribution],
      place: &Place,
   ) -> Vec<Draft> {
      let Some(template) = template else {
         return Vec::new();
      };
      contributions
         .iter()
         .map(|contribution| {
            let mut button = Draft::from_node(template);
            button.set(
               Prop::ACCESSIBILITY_ID,
               Some(Value::Str(format!(
                  "phoenix.{}.{}",
                  contribution.plugin, contribution.id.0
               ))),
            );
            button.set(
               Prop::ACCESSIBILITY_LABEL,
               Some(Value::Str(contribution.label.clone())),
            );
            if let (Some(icon), Some(image)) = (&contribution.icon, button.kids.first_mut()) {
               image.set(Prop::URI, Some(Value::Str(icon.clone())));
            }
            button.press = Some(Handler::Press {
               contribution: contribution.id,
               place:        place.clone(),
               dismiss:      false,
            });
            button
         })
         .collect()
   }

   fn action_group(
      &self,
      contributions: &[&Contribution],
      place: &Place,
      outcome: &impl Fn(&Contribution) -> Option<Outcome>,
   ) -> Vec<Draft> {
      let (Some(row), Some((group_kind, group_props))) = (&self.row, &self.group) else {
         return Vec::new();
      };
      let shown = contributions
         .iter()
         .filter(|contribution| {
            !contribution.conditional || matches!(outcome(contribution), Some(Outcome::Shown(true)))
         })
         .collect::<Vec<_>>();
      let count = shown.len();
      let mut kids = Vec::new();
      for (index, contribution) in shown.into_iter().enumerate() {
         let mut draft = Draft::from_node(row);
         for corner in [
            Prop::RADIUS,
            Prop::RADIUS_TOP_LEFT,
            Prop::RADIUS_TOP_RIGHT,
            Prop::RADIUS_BOTTOM_LEFT,
            Prop::RADIUS_BOTTOM_RIGHT,
         ] {
            draft.set(corner, None);
         }
         let round = Some(Value::F64(12.0));
         if count == 1 {
            draft.set(Prop::RADIUS, round);
         } else if index == 0 {
            draft.set(Prop::RADIUS_TOP_LEFT, round.clone());
            draft.set(Prop::RADIUS_TOP_RIGHT, round);
         } else if index == count - 1 {
            draft.set(Prop::RADIUS_BOTTOM_LEFT, round.clone());
            draft.set(Prop::RADIUS_BOTTOM_RIGHT, round);
         }
         draft.set(
            Prop::ACCESSIBILITY_ID,
            Some(Value::Str(format!(
               "phoenix.{}.{}",
               contribution.plugin, contribution.id.0
            ))),
         );
         draft.set(
            Prop::ACCESSIBILITY_LABEL,
            Some(Value::Str(contribution.label.clone())),
         );
         let inner = &mut draft.kids[0];
         let icon = contribution.icon.as_deref().unwrap_or(DEFAULT_ICON);
         inner.kids[0].set(Prop::URI, Some(Value::Str(icon.to_owned())));
         inner.kids[1].set(Prop::TEXT, Some(Value::Str(contribution.label.clone())));
         if contribution.danger {
            inner.kids[1].set(Prop::COLOR, Some(Value::I64(DANGER)));
            inner.kids[0].set(Prop::TINT, Some(Value::I64(DANGER)));
         }
         draft.press = Some(Handler::Press {
            contribution: contribution.id,
            place:        place.clone(),
            dismiss:      true,
         });
         if index > 0
            && let Some(divider) = &self.divider
         {
            kids.push(Draft::from_node(divider));
         }
         kids.push(draft);
      }
      if kids.is_empty() {
         return Vec::new();
      }
      vec![Draft {
         kind: group_kind.clone(),
         props: group_props.clone(),
         kids,
         press: None,
      }]
   }

   /// The timestamp's text style without its placement, or a fallback.
   fn accessory_props(&self) -> Props {
      const DROPPED: [Prop; 7] = [
         Prop::ACCESSIBILITY_ID,
         Prop::ACCESSIBILITY_LABEL,
         Prop::named("accessibilityHidden"),
         Prop::named("maxLines"),
         Prop::MARGIN,
         Prop::named("gravity"),
         Prop::ON,
      ];
      self.timestamp.as_ref().map_or_else(
         || {
            vec![
               (Prop::COLOR, Value::I64(MUTED)),
               (Prop::FONT_SIZE, Value::F64(12.0)),
               (Prop::named("fontWeight"), Value::I32(500)),
            ]
         },
         |timestamp| {
            timestamp
               .props
               .iter()
               .filter(|(key, _)| !DROPPED.contains(key))
               .cloned()
               .collect()
         },
      )
   }
}

fn text(props: &Props, rendered: &Rendered) -> Draft {
   let mut node = Draft {
      kind:  ViewKind::Text,
      props: props.clone(),
      kids:  Vec::new(),
      press: None,
   };
   node.set(Prop::named("spans"), None);
   node.set(Prop::TEXT, Some(Value::Str(rendered.text.clone())));
   if let Some(color) = rendered.color {
      node.set(Prop::COLOR, Some(Value::I64(color)));
   }
   node
}
