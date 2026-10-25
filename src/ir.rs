//! The compact internal IR executed by the interpreter.
//!
//! Function bodies are lowered once, at validation time, from structured
//! WebAssembly control flow into a flat array of [`Op`]s:
//!
//! * `block`/`loop`/`if`/`else`/`end` disappear; branches carry an absolute
//!   target index and a precomputed [`DropKeep`] that tells the interpreter how
//!   to unwind the operand stack, so no control stack exists at run time.
//! * Values are untyped 64-bit slots. Loads, stores and constants therefore
//!   collapse by width (`f32.load` and `i32.load` are the same op).
//! * Locals are addressed relative to the frame pointer.
//!
//! Every `Op` is 16 bytes (asserted below).

use std::fmt;

use crate::ops::NumOp;

/// How to unwind the operand stack on a taken branch: keep the top `keep`
/// values and discard the `drop` values beneath them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DropKeep {
    /// Slots discarded below the kept values.
    pub drop: u32,
    /// Slots carried to the branch target.
    pub keep: u32,
}

macro_rules! define_ir {
    ($($name:ident $code:literal [$($p:ident)*] $r:ident,)*) => {
        /// One interpreter instruction.
        #[derive(Debug, Clone, Copy, PartialEq)]
        #[allow(missing_docs)]
        pub enum Op {
            Unreachable,
            /// Unconditional jump.
            Br { target: u32, dk: DropKeep },
            /// Pops an i32; jumps when it is non-zero.
            BrIfNez { target: u32, dk: DropKeep },
            /// Pops an i32; jumps (without unwinding) when it is zero. Used for `if`.
            BrIfEqz { target: u32 },
            /// Pops an index; followed by `len + 1` [`Op::BrTarget`] entries,
            /// the last being the default.
            BrTable { len: u32 },
            /// A `br_table` entry; never executed directly.
            BrTarget { target: u32, dk: DropKeep },
            /// Returns the top `keep` values to the caller.
            Return { keep: u32 },
            Call(u32),
            CallIndirect { type_idx: u32, table: u32 },
            Drop,
            Select,
            LocalGet(u32),
            LocalSet(u32),
            LocalTee(u32),
            GlobalGet(u32),
            GlobalSet(u32),
            TableGet(u32),
            TableSet(u32),
            TableSize(u32),
            TableGrow(u32),
            TableFill(u32),
            TableCopy { dst: u32, src: u32 },
            TableInit { elem: u32, table: u32 },
            ElemDrop(u32),
            /// Loads carry the static offset. Signed loads sign-extend to 64
            /// bits, which is also a valid i32 because i32 reads truncate.
            Load8S(u32),
            Load8U(u32),
            Load16S(u32),
            Load16U(u32),
            Load32S(u32),
            Load32U(u32),
            Load64(u32),
            Store8(u32),
            Store16(u32),
            Store32(u32),
            Store64(u32),
            MemorySize,
            MemoryGrow,
            MemoryInit(u32),
            DataDrop(u32),
            MemoryCopy,
            MemoryFill,
            /// Any constant: integers, float bits, or a null reference (0).
            Const(u64),
            RefIsNull,
            RefFunc(u32),

            // ---- superinstructions (see `crate::fuse`) ----
            /// `local.get a; local.get b`
            LocalGet2(u32, u32),
            /// `local.get src; i32.const imm; i32.add; local.set dst`
            /// (`i32.sub` is folded in with a negated immediate).
            I32LocalAddImm { dst: u32, src: u32, imm: i32 },
            /// As [`Op::I32LocalAddImm`] for i64, with a sign-extended immediate.
            I64LocalAddImm { dst: u32, src: u32, imm: i32 },
            /// `i32.const imm; i32.add` (and `i32.sub` with `-imm`).
            I32AddImm(i32),
            /// `i32.const imm; i32.mul`
            I32MulImm(i32),
            /// `i32.const imm; i32.and`
            I32AndImm(i32),
            /// `i32.const imm; i32.shl` (shift pre-masked to 0..=31)
            I32ShlImm(u32),
            /// `i32.const imm; i32.shr_s`
            I32ShrSImm(u32),
            /// `i32.const imm; i32.shr_u`
            I32ShrUImm(u32),
            /// `i64.const imm; i64.add` (and `i64.sub` with `-imm`).
            I64AddImm(i64),
            /// `i64.const imm; i64.mul`
            I64MulImm(i64),
            /// `i64.const imm; i64.and`
            I64AndImm(i64),
            /// `i64.const imm; i64.shl` (shift pre-masked to 0..=63)
            I64ShlImm(u32),
            /// `i64.const imm; i64.shr_s`
            I64ShrSImm(u32),
            /// `i64.const imm; i64.shr_u`
            I64ShrUImm(u32),
            /// Compare-and-branch: pops two operands and jumps to `target`
            /// when the comparison holds. Produced from `<cmp>; br_if` (with
            /// nothing to unwind) and from `<cmp>; if` (with the comparison
            /// inverted).
            BrIfI32Eq { target: u32 },
            BrIfI32Ne { target: u32 },
            BrIfI32LtS { target: u32 },
            BrIfI32LtU { target: u32 },
            BrIfI32GtS { target: u32 },
            BrIfI32GtU { target: u32 },
            BrIfI32LeS { target: u32 },
            BrIfI32LeU { target: u32 },
            BrIfI32GeS { target: u32 },
            BrIfI32GeU { target: u32 },
            BrIfI64Eq { target: u32 },
            BrIfI64Ne { target: u32 },
            BrIfI64LtS { target: u32 },
            BrIfI64LtU { target: u32 },
            BrIfI64GtS { target: u32 },
            BrIfI64GtU { target: u32 },
            BrIfI64LeS { target: u32 },
            BrIfI64LeU { target: u32 },
            BrIfI64GeS { target: u32 },
            BrIfI64GeU { target: u32 },
            /// Pops an i64; jumps when it is zero.
            BrIfI64Eqz { target: u32 },
            /// Pops an i64; jumps when it is non-zero.
            BrIfI64Nez { target: u32 },
            $($name,)*
        }

        impl Op {
            /// The IR op implementing a numeric instruction.
            pub fn from_num(n: NumOp) -> Op {
                match n { $(NumOp::$name => Op::$name,)* }
            }
        }
    };
}

crate::for_each_numop!(define_ir);

const _: () = assert!(std::mem::size_of::<Op>() == 16);

macro_rules! with_targets {
    ($self:ident, $t:ident => $e:expr, $other:expr) => {
        match $self {
            Op::Br { target: $t, .. }
            | Op::BrIfNez { target: $t, .. }
            | Op::BrIfEqz { target: $t }
            | Op::BrTarget { target: $t, .. }
            | Op::BrIfI32Eq { target: $t }
            | Op::BrIfI32Ne { target: $t }
            | Op::BrIfI32LtS { target: $t }
            | Op::BrIfI32LtU { target: $t }
            | Op::BrIfI32GtS { target: $t }
            | Op::BrIfI32GtU { target: $t }
            | Op::BrIfI32LeS { target: $t }
            | Op::BrIfI32LeU { target: $t }
            | Op::BrIfI32GeS { target: $t }
            | Op::BrIfI32GeU { target: $t }
            | Op::BrIfI64Eq { target: $t }
            | Op::BrIfI64Ne { target: $t }
            | Op::BrIfI64LtS { target: $t }
            | Op::BrIfI64LtU { target: $t }
            | Op::BrIfI64GtS { target: $t }
            | Op::BrIfI64GtU { target: $t }
            | Op::BrIfI64LeS { target: $t }
            | Op::BrIfI64LeU { target: $t }
            | Op::BrIfI64GeS { target: $t }
            | Op::BrIfI64GeU { target: $t }
            | Op::BrIfI64Eqz { target: $t }
            | Op::BrIfI64Nez { target: $t } => $e,
            _ => $other,
        }
    };
}

impl Op {
    /// The code index this op may jump to, if it is a branch (including
    /// `br_table` entries).
    pub fn branch_target(&self) -> Option<u32> {
        with_targets!(self, t => Some(*t), None)
    }

    /// Mutable access to the jump target, if this op is a branch.
    pub(crate) fn branch_target_mut(&mut self) -> Option<&mut u32> {
        with_targets!(self, t => Some(t), None)
    }
}

/// A function body after lowering.
#[derive(Debug, Clone)]
pub struct CompiledFunc {
    /// Type index (into the defining module's type section).
    pub type_idx: u32,
    /// Number of parameters.
    pub params: u32,
    /// Number of non-parameter locals (zero-initialised on entry).
    pub locals: u32,
    /// Number of results.
    pub results: u32,
    /// Maximum operand-stack depth reached by the body.
    pub max_stack: u32,
    /// The lowered code.
    pub code: Box<[Op]>,
}

impl CompiledFunc {
    /// Stack slots the frame needs beyond its parameters.
    pub fn frame_size(&self) -> usize {
        self.locals as usize + self.max_stack as usize
    }
}

impl fmt::Display for CompiledFunc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "  ; params={} locals={} results={} max_stack={} ops={}",
            self.params,
            self.locals,
            self.results,
            self.max_stack,
            self.code.len()
        )?;
        for (pc, op) in self.code.iter().enumerate() {
            writeln!(f, "  {pc:4}: {}", DisplayOp(op))?;
        }
        Ok(())
    }
}

struct DisplayOp<'a>(&'a Op);

impl fmt::Display for DisplayOp<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let dk = |dk: &DropKeep| {
            if dk.drop == 0 {
                String::new()
            } else {
                format!(" (drop {} keep {})", dk.drop, dk.keep)
            }
        };
        match self.0 {
            Op::Br { target, dk: d } => write!(f, "br @{target}{}", dk(d)),
            Op::BrIfNez { target, dk: d } => write!(f, "br_if_nez @{target}{}", dk(d)),
            Op::BrIfEqz { target } => write!(f, "br_if_eqz @{target}"),
            Op::BrTarget { target, dk: d } => write!(f, "  -> @{target}{}", dk(d)),
            Op::Const(v) => write!(f, "const {v:#x}"),
            op if op.branch_target().is_some() => {
                let s = format!("{op:?}");
                let name = s.split(' ').next().unwrap_or_default();
                write!(f, "{name} @{}", op.branch_target().unwrap_or_default())
            }
            other => {
                let s = format!("{other:?}");
                f.write_str(&s)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_num_maps_every_numeric_op() {
        for &n in NumOp::ALL {
            let op = Op::from_num(n);
            assert_eq!(format!("{op:?}"), n.name());
        }
    }

    #[test]
    fn disassembly() {
        let f = CompiledFunc {
            type_idx: 0,
            params: 1,
            locals: 0,
            results: 1,
            max_stack: 2,
            code: vec![
                Op::LocalGet(0),
                Op::Const(1),
                Op::I32Add,
                Op::Br {
                    target: 5,
                    dk: DropKeep { drop: 1, keep: 1 },
                },
                Op::BrIfEqz { target: 5 },
                Op::Return { keep: 1 },
            ]
            .into(),
        };
        let s = f.to_string();
        assert!(s.contains("params=1"));
        assert!(s.contains("3: br @5 (drop 1 keep 1)"));
        assert!(s.contains("4: br_if_eqz @5"));
        assert!(s.contains("1: const 0x1"));
        assert!(s.contains("Return { keep: 1 }"));
        assert_eq!(f.frame_size(), 2);
    }

    #[test]
    fn fused_branches_expose_and_print_targets() {
        let mut op = Op::BrIfI64LtU { target: 3 };
        assert_eq!(op.branch_target(), Some(3));
        *op.branch_target_mut().unwrap() = 9;
        assert_eq!(DisplayOp(&op).to_string(), "BrIfI64LtU @9");
        assert_eq!(Op::LocalGet2(1, 2).branch_target(), None);
        assert_eq!(DisplayOp(&Op::I32AddImm(-1)).to_string(), "I32AddImm(-1)");
    }
}
