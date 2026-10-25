//! Typed values at the embedding boundary and their untyped slot encoding.

use std::fmt;

use crate::types::{RefType, ValType};

/// A handle to a function in a [`crate::Store`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Func(pub(crate) u32);

/// A typed WebAssembly value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Value {
    /// `i32`
    I32(i32),
    /// `i64`
    I64(i64),
    /// `f32` (bit patterns, including NaN payloads, are preserved)
    F32(f32),
    /// `f64` (bit patterns, including NaN payloads, are preserved)
    F64(f64),
    /// `funcref` (`None` is `ref.null func`)
    FuncRef(Option<Func>),
    /// `externref` (`None` is `ref.null extern`); the payload is an opaque host id.
    ExternRef(Option<u32>),
}

impl Value {
    /// The type of this value.
    pub fn ty(&self) -> ValType {
        match self {
            Value::I32(_) => ValType::I32,
            Value::I64(_) => ValType::I64,
            Value::F32(_) => ValType::F32,
            Value::F64(_) => ValType::F64,
            Value::FuncRef(_) => ValType::FUNCREF,
            Value::ExternRef(_) => ValType::EXTERNREF,
        }
    }

    /// The zero value / null reference of a type.
    pub fn default_for(t: ValType) -> Value {
        from_slot(0, t)
    }

    /// Returns the i32 payload, if this is an i32.
    pub fn as_i32(&self) -> Option<i32> {
        match self {
            Value::I32(v) => Some(*v),
            _ => None,
        }
    }

    /// Returns the i64 payload, if this is an i64.
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::I64(v) => Some(*v),
            _ => None,
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::I32(v) => write!(f, "{v}:i32"),
            Value::I64(v) => write!(f, "{v}:i64"),
            Value::F32(v) => write!(f, "{v}:f32"),
            Value::F64(v) => write!(f, "{v}:f64"),
            Value::FuncRef(None) | Value::ExternRef(None) => write!(f, "null:{}", self.ty()),
            Value::FuncRef(Some(Func(a))) => write!(f, "func#{a}"),
            Value::ExternRef(Some(h)) => write!(f, "extern#{h}"),
        }
    }
}

/// Encodes a value into a 64-bit stack slot. References encode as
/// `address + 1` so that 0 is null.
pub(crate) fn to_slot(v: Value) -> u64 {
    match v {
        Value::I32(x) => u64::from(x as u32),
        Value::I64(x) => x as u64,
        Value::F32(x) => u64::from(x.to_bits()),
        Value::F64(x) => x.to_bits(),
        Value::FuncRef(r) => r.map_or(0, |Func(a)| u64::from(a) + 1),
        Value::ExternRef(r) => r.map_or(0, |h| u64::from(h) + 1),
    }
}

/// Decodes a slot given its static type.
pub(crate) fn from_slot(s: u64, t: ValType) -> Value {
    match t {
        ValType::I32 => Value::I32(s as u32 as i32),
        ValType::I64 => Value::I64(s as i64),
        ValType::F32 => Value::F32(f32::from_bits(s as u32)),
        ValType::F64 => Value::F64(f64::from_bits(s)),
        ValType::Ref(RefType::Func) => Value::FuncRef(ref_addr(s).map(Func)),
        ValType::Ref(RefType::Extern) => Value::ExternRef(ref_addr(s)),
    }
}

/// The address encoded in a non-null reference slot.
pub(crate) fn ref_addr(s: u64) -> Option<u32> {
    if s == 0 {
        None
    } else {
        Some((s - 1) as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_roundtrip_preserves_bits() {
        let nan = f32::from_bits(0x7fa0_0001);
        let values = [
            Value::I32(-1),
            Value::I64(i64::MIN),
            Value::F32(nan),
            Value::F64(f64::from_bits(0xfff8_0000_0000_0001)),
            Value::FuncRef(None),
            Value::FuncRef(Some(Func(7))),
            Value::ExternRef(Some(0)),
            Value::ExternRef(None),
        ];
        for v in values {
            let back = from_slot(to_slot(v), v.ty());
            match (v, back) {
                (Value::F32(a), Value::F32(b)) => assert_eq!(a.to_bits(), b.to_bits()),
                (Value::F64(a), Value::F64(b)) => assert_eq!(a.to_bits(), b.to_bits()),
                _ => assert_eq!(v, back),
            }
        }
        assert_eq!(to_slot(Value::I32(-1)), 0xffff_ffff);
    }

    #[test]
    fn defaults_and_display() {
        assert_eq!(Value::default_for(ValType::I64), Value::I64(0));
        assert_eq!(Value::default_for(ValType::FUNCREF), Value::FuncRef(None));
        assert_eq!(Value::I32(5).to_string(), "5:i32");
        assert_eq!(Value::ExternRef(None).to_string(), "null:externref");
        assert_eq!(Value::FuncRef(Some(Func(2))).to_string(), "func#2");
        assert_eq!(Value::ExternRef(Some(9)).to_string(), "extern#9");
        assert_eq!(Value::F64(1.5).to_string(), "1.5:f64");
        assert_eq!(Value::I32(3).as_i32(), Some(3));
        assert_eq!(Value::I64(3).as_i32(), None);
        assert_eq!(Value::I64(4).as_i64(), Some(4));
        assert_eq!(Value::F32(1.0).as_i64(), None);
    }
}
