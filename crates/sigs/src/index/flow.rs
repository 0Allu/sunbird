//! Register values through a function's steps, walked forward once.
//!
//! - A call clobbers the ISA's caller-saved registers.
//! - A register an instruction writes without its [`Op`] saying how becomes
//!   [`Val::Unknown`].
//! - A branch target starts from what every earlier branch to it agrees on, met
//!   with the fall-through unless an unconditional jump or return ends it. Back
//!   edges and jump tables are not followed.

use std::collections::HashMap;

use crate::index::{
   Isa,
   Op,
   Reg,
   Step,
};

/// What a register holds before a step, as far as the walk can tell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Val {
   #[default]
   Unknown,
   /// The stack pointer at entry plus this.
   Stack(isize),
   /// The word loaded from the stack slot at entry `sp` plus `slot`, plus
   /// `bias`.
   Spilled {
      slot: isize,
      bias: isize,
   },
   Addr(usize),
   Imm(u64),
}

impl Val {
   const fn offset(self, by: isize) -> Self {
      match self {
         Self::Stack(at) => Self::Stack(at + by),
         Self::Spilled { slot, bias } => {
            Self::Spilled {
               slot,
               bias: bias + by,
            }
         },
         Self::Addr(at) => Self::Addr(at.wrapping_add_signed(by)),
         Self::Unknown | Self::Imm(_) => Self::Unknown,
      }
   }

   const fn loaded(self, offset: isize) -> Self {
      match self {
         Self::Stack(at) => {
            Self::Spilled {
               slot: at + offset,
               bias: 0,
            }
         },
         _ => Self::Unknown,
      }
   }
}

/// Each register's value, indexed by [`Reg`].
pub type Regs = [Val; 32];

fn meet(into: &mut Regs, other: &Regs) {
   for (mine, theirs) in into.iter_mut().zip(other) {
      if mine != theirs {
         *mine = Val::Unknown;
      }
   }
}

/// Calls `visit(index, step, regs)` for each step with the registers before
/// it, `sp` starting at `Val::Stack(0)`.
pub fn walk(isa: Isa, steps: &[Step], mut visit: impl FnMut(usize, &Step, &Regs)) {
   let sp = usize::from(isa.sp());
   let mut regs = [Val::Unknown; 32];
   regs[sp] = Val::Stack(0);
   let mut incoming = HashMap::<usize, Regs>::new();
   let mut ended = false;
   for (index, step) in steps.iter().enumerate() {
      if let Some(joined) = incoming.remove(&step.at) {
         if ended {
            regs = joined;
         } else {
            meet(&mut regs, &joined);
         }
      }
      ended = false;
      visit(index, step, &regs);

      let before = regs;
      for (reg, val) in regs.iter_mut().enumerate() {
         if step.writes & 1 << reg != 0 {
            *val = Val::Unknown;
         }
      }
      let mut set = |reg: Reg, val: Val| regs[usize::from(reg)] = val;
      let of = |reg: Reg| before[usize::from(reg)];
      match step.op {
         Op::Addr { reg, target } => set(reg, Val::Addr(target)),
         Op::Imm { reg, value } => set(reg, Val::Imm(value)),
         Op::Move { dst, src } => set(dst, of(src)),
         Op::Offset { reg, base, offset } => set(reg, of(base).offset(offset)),
         Op::Load {
            reg, base, offset, ..
         } => set(reg, of(base).loaded(offset)),
         Op::LoadPair {
            regs: [first, second],
            base,
            offset,
            size,
         } => {
            set(first, of(base).loaded(offset));
            set(second, of(base).loaded(offset + isize::from(size)));
         },
         Op::Call(_) => {
            for (reg, val) in regs.iter_mut().enumerate() {
               if isa.caller_saved() & 1 << reg != 0 {
                  *val = Val::Unknown;
               }
            }
         },
         Op::Branch(target) => {
            incoming
               .entry(target)
               .and_modify(|joined| meet(joined, &regs))
               .or_insert(regs);
         },
         Op::Jump(target) => {
            incoming
               .entry(target)
               .and_modify(|joined| meet(joined, &regs))
               .or_insert(regs);
            ended = true;
         },
         Op::Ret => ended = true,
         _ => {},
      }
      if let Some((reg, by)) = step.writeback {
         regs[usize::from(reg)] = before[usize::from(reg)].offset(by);
      }
   }
}
