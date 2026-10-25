//! The numeric instruction table.
//!
//! Every numeric instruction that takes no immediates is listed exactly once
//! in [`for_each_numop!`] together with its opcode and type signature. Other
//! modules generate the decoder table, the validator signatures and the IR
//! variants from this single list, so they cannot drift apart.

use crate::types::ValType;

/// Invokes `$m!` with the full numeric-op table:
/// `Name opcode [param types] result_type,` per line. Opcodes `0xFCxx` are
/// the `0xFC`-prefixed saturating truncations.
#[macro_export]
#[doc(hidden)]
macro_rules! for_each_numop {
    ($m:ident) => {
        $m! {
            I32Eqz 0x45 [I32] I32,
            I32Eq 0x46 [I32 I32] I32,
            I32Ne 0x47 [I32 I32] I32,
            I32LtS 0x48 [I32 I32] I32,
            I32LtU 0x49 [I32 I32] I32,
            I32GtS 0x4a [I32 I32] I32,
            I32GtU 0x4b [I32 I32] I32,
            I32LeS 0x4c [I32 I32] I32,
            I32LeU 0x4d [I32 I32] I32,
            I32GeS 0x4e [I32 I32] I32,
            I32GeU 0x4f [I32 I32] I32,
            I64Eqz 0x50 [I64] I32,
            I64Eq 0x51 [I64 I64] I32,
            I64Ne 0x52 [I64 I64] I32,
            I64LtS 0x53 [I64 I64] I32,
            I64LtU 0x54 [I64 I64] I32,
            I64GtS 0x55 [I64 I64] I32,
            I64GtU 0x56 [I64 I64] I32,
            I64LeS 0x57 [I64 I64] I32,
            I64LeU 0x58 [I64 I64] I32,
            I64GeS 0x59 [I64 I64] I32,
            I64GeU 0x5a [I64 I64] I32,
            F32Eq 0x5b [F32 F32] I32,
            F32Ne 0x5c [F32 F32] I32,
            F32Lt 0x5d [F32 F32] I32,
            F32Gt 0x5e [F32 F32] I32,
            F32Le 0x5f [F32 F32] I32,
            F32Ge 0x60 [F32 F32] I32,
            F64Eq 0x61 [F64 F64] I32,
            F64Ne 0x62 [F64 F64] I32,
            F64Lt 0x63 [F64 F64] I32,
            F64Gt 0x64 [F64 F64] I32,
            F64Le 0x65 [F64 F64] I32,
            F64Ge 0x66 [F64 F64] I32,
            I32Clz 0x67 [I32] I32,
            I32Ctz 0x68 [I32] I32,
            I32Popcnt 0x69 [I32] I32,
            I32Add 0x6a [I32 I32] I32,
            I32Sub 0x6b [I32 I32] I32,
            I32Mul 0x6c [I32 I32] I32,
            I32DivS 0x6d [I32 I32] I32,
            I32DivU 0x6e [I32 I32] I32,
            I32RemS 0x6f [I32 I32] I32,
            I32RemU 0x70 [I32 I32] I32,
            I32And 0x71 [I32 I32] I32,
            I32Or 0x72 [I32 I32] I32,
            I32Xor 0x73 [I32 I32] I32,
            I32Shl 0x74 [I32 I32] I32,
            I32ShrS 0x75 [I32 I32] I32,
            I32ShrU 0x76 [I32 I32] I32,
            I32Rotl 0x77 [I32 I32] I32,
            I32Rotr 0x78 [I32 I32] I32,
            I64Clz 0x79 [I64] I64,
            I64Ctz 0x7a [I64] I64,
            I64Popcnt 0x7b [I64] I64,
            I64Add 0x7c [I64 I64] I64,
            I64Sub 0x7d [I64 I64] I64,
            I64Mul 0x7e [I64 I64] I64,
            I64DivS 0x7f [I64 I64] I64,
            I64DivU 0x80 [I64 I64] I64,
            I64RemS 0x81 [I64 I64] I64,
            I64RemU 0x82 [I64 I64] I64,
            I64And 0x83 [I64 I64] I64,
            I64Or 0x84 [I64 I64] I64,
            I64Xor 0x85 [I64 I64] I64,
            I64Shl 0x86 [I64 I64] I64,
            I64ShrS 0x87 [I64 I64] I64,
            I64ShrU 0x88 [I64 I64] I64,
            I64Rotl 0x89 [I64 I64] I64,
            I64Rotr 0x8a [I64 I64] I64,
            F32Abs 0x8b [F32] F32,
            F32Neg 0x8c [F32] F32,
            F32Ceil 0x8d [F32] F32,
            F32Floor 0x8e [F32] F32,
            F32Trunc 0x8f [F32] F32,
            F32Nearest 0x90 [F32] F32,
            F32Sqrt 0x91 [F32] F32,
            F32Add 0x92 [F32 F32] F32,
            F32Sub 0x93 [F32 F32] F32,
            F32Mul 0x94 [F32 F32] F32,
            F32Div 0x95 [F32 F32] F32,
            F32Min 0x96 [F32 F32] F32,
            F32Max 0x97 [F32 F32] F32,
            F32Copysign 0x98 [F32 F32] F32,
            F64Abs 0x99 [F64] F64,
            F64Neg 0x9a [F64] F64,
            F64Ceil 0x9b [F64] F64,
            F64Floor 0x9c [F64] F64,
            F64Trunc 0x9d [F64] F64,
            F64Nearest 0x9e [F64] F64,
            F64Sqrt 0x9f [F64] F64,
            F64Add 0xa0 [F64 F64] F64,
            F64Sub 0xa1 [F64 F64] F64,
            F64Mul 0xa2 [F64 F64] F64,
            F64Div 0xa3 [F64 F64] F64,
            F64Min 0xa4 [F64 F64] F64,
            F64Max 0xa5 [F64 F64] F64,
            F64Copysign 0xa6 [F64 F64] F64,
            I32WrapI64 0xa7 [I64] I32,
            I32TruncF32S 0xa8 [F32] I32,
            I32TruncF32U 0xa9 [F32] I32,
            I32TruncF64S 0xaa [F64] I32,
            I32TruncF64U 0xab [F64] I32,
            I64ExtendI32S 0xac [I32] I64,
            I64ExtendI32U 0xad [I32] I64,
            I64TruncF32S 0xae [F32] I64,
            I64TruncF32U 0xaf [F32] I64,
            I64TruncF64S 0xb0 [F64] I64,
            I64TruncF64U 0xb1 [F64] I64,
            F32ConvertI32S 0xb2 [I32] F32,
            F32ConvertI32U 0xb3 [I32] F32,
            F32ConvertI64S 0xb4 [I64] F32,
            F32ConvertI64U 0xb5 [I64] F32,
            F32DemoteF64 0xb6 [F64] F32,
            F64ConvertI32S 0xb7 [I32] F64,
            F64ConvertI32U 0xb8 [I32] F64,
            F64ConvertI64S 0xb9 [I64] F64,
            F64ConvertI64U 0xba [I64] F64,
            F64PromoteF32 0xbb [F32] F64,
            I32ReinterpretF32 0xbc [F32] I32,
            I64ReinterpretF64 0xbd [F64] I64,
            F32ReinterpretI32 0xbe [I32] F32,
            F64ReinterpretI64 0xbf [I64] F64,
            I32Extend8S 0xc0 [I32] I32,
            I32Extend16S 0xc1 [I32] I32,
            I64Extend8S 0xc2 [I64] I64,
            I64Extend16S 0xc3 [I64] I64,
            I64Extend32S 0xc4 [I64] I64,
            I32TruncSatF32S 0xfc00 [F32] I32,
            I32TruncSatF32U 0xfc01 [F32] I32,
            I32TruncSatF64S 0xfc02 [F64] I32,
            I32TruncSatF64U 0xfc03 [F64] I32,
            I64TruncSatF32S 0xfc04 [F32] I64,
            I64TruncSatF32U 0xfc05 [F32] I64,
            I64TruncSatF64S 0xfc06 [F64] I64,
            I64TruncSatF64U 0xfc07 [F64] I64,
        }
    };
}

macro_rules! define_numop {
    ($($name:ident $code:literal [$($p:ident)*] $r:ident,)*) => {
        /// A numeric instruction without immediates.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        #[allow(missing_docs)]
        pub enum NumOp { $($name,)* }

        impl NumOp {
            /// Every numeric op, in opcode order.
            pub const ALL: &'static [NumOp] = &[$(NumOp::$name,)*];

            /// Looks up an opcode (`0xFCxx` for the prefixed ones).
            pub fn from_opcode(code: u16) -> Option<NumOp> {
                match code {
                    $($code => Some(NumOp::$name),)*
                    _ => None,
                }
            }

            /// The opcode of this instruction.
            pub fn opcode(self) -> u16 {
                match self { $(NumOp::$name => $code,)* }
            }

            /// Operand types (bottom of stack first) and the result type.
            pub fn signature(self) -> (&'static [ValType], ValType) {
                match self {
                    $(NumOp::$name => (&[$(ValType::$p),*], ValType::$r),)*
                }
            }

            /// The variant name, e.g. `I32Add`.
            pub fn name(self) -> &'static str {
                match self { $(NumOp::$name => stringify!($name),)* }
            }
        }
    };
}

for_each_numop!(define_numop);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_roundtrips_and_is_dense() {
        assert_eq!(NumOp::ALL.len(), 0xc4 - 0x45 + 1 + 8);
        for &op in NumOp::ALL {
            assert_eq!(NumOp::from_opcode(op.opcode()), Some(op));
        }
        assert_eq!(NumOp::from_opcode(0x6a), Some(NumOp::I32Add));
        assert_eq!(NumOp::from_opcode(0x44), None);
        assert_eq!(NumOp::from_opcode(0xfc08), None);
        let (params, result) = NumOp::F64ConvertI64U.signature();
        assert_eq!(params, &[ValType::I64]);
        assert_eq!(result, ValType::F64);
        assert_eq!(NumOp::I64Rotr.name(), "I64Rotr");
    }
}
