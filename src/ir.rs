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
}
