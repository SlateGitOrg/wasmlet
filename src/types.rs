//! Core WebAssembly types shared by the decoder, validator and runtime.

use std::fmt;

/// A reference type (`funcref` or `externref`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RefType {
    /// A nullable reference to a function.
    Func,
    /// A nullable opaque host reference.
    Extern,
}

/// A value type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValType {
    /// 32-bit integer.
    I32,
    /// 64-bit integer.
    I64,
    /// 32-bit IEEE-754 float.
    F32,
    /// 64-bit IEEE-754 float.
    F64,
    /// A reference value.
    Ref(RefType),
}

impl ValType {
    /// `funcref` shorthand.
    pub const FUNCREF: ValType = ValType::Ref(RefType::Func);
    /// `externref` shorthand.
    pub const EXTERNREF: ValType = ValType::Ref(RefType::Extern);

    /// Whether this is a numeric (non-reference) type.
    pub fn is_num(self) -> bool {
        !matches!(self, ValType::Ref(_))
    }
}

impl fmt::Display for RefType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            RefType::Func => "funcref",
            RefType::Extern => "externref",
        })
    }
}

impl fmt::Display for ValType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ValType::I32 => f.write_str("i32"),
            ValType::I64 => f.write_str("i64"),
            ValType::F32 => f.write_str("f32"),
            ValType::F64 => f.write_str("f64"),
            ValType::Ref(r) => r.fmt(f),
        }
    }
}

/// A function signature: parameter and result types.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct FuncType {
    params: Box<[ValType]>,
    results: Box<[ValType]>,
}

impl FuncType {
    /// Builds a function type from parameter and result lists.
    pub fn new(
        params: impl IntoIterator<Item = ValType>,
        results: impl IntoIterator<Item = ValType>,
    ) -> Self {
        FuncType {
            params: params.into_iter().collect(),
            results: results.into_iter().collect(),
        }
    }

    /// Parameter types.
    pub fn params(&self) -> &[ValType] {
        &self.params
    }

    /// Result types.
    pub fn results(&self) -> &[ValType] {
        &self.results
    }
}

impl fmt::Display for FuncType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let list = |xs: &[ValType]| {
            xs.iter()
                .map(|t| t.to_string())
                .collect::<Vec<_>>()
                .join(" ")
        };
        write!(f, "[{}] -> [{}]", list(&self.params), list(&self.results))
    }
}

/// Size limits of a table or memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Initial size.
    pub min: u32,
    /// Optional maximum size.
    pub max: Option<u32>,
}

impl Limits {
    /// Import subtyping (spec "limits matching"): `self` (the provided limits)
    /// matches `required` if it is at least as large and at most as permissive.
    pub fn matches(&self, required: &Limits) -> bool {
        if self.min < required.min {
            return false;
        }
        match (self.max, required.max) {
            (_, None) => true,
            (None, Some(_)) => false,
            (Some(a), Some(b)) => a <= b,
        }
    }
}

/// A table type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableType {
    /// Element reference type.
    pub elem: RefType,
    /// Size limits in elements.
    pub limits: Limits,
}

/// A linear memory type (sizes in 64 KiB pages).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryType {
    /// Size limits in pages.
    pub limits: Limits,
}

/// Global mutability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mutability {
    /// Immutable (`const`).
    Const,
    /// Mutable (`var`).
    Var,
}

/// A global type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlobalType {
    /// Value type of the global.
    pub content: ValType,
    /// Whether the global can be written.
    pub mutability: Mutability,
}

/// The type of a structured control instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockType {
    /// `[] -> []`
    Empty,
    /// `[] -> [t]`
    Value(ValType),
    /// A type-section index (multi-value).
    Func(u32),
}

/// Kind of an external value (import/export descriptor).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternKind {
    /// A function.
    Func,
    /// A table.
    Table,
    /// A memory.
    Memory,
    /// A global.
    Global,
}

impl fmt::Display for ExternKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ExternKind::Func => "func",
            ExternKind::Table => "table",
            ExternKind::Memory => "memory",
            ExternKind::Global => "global",
        })
    }
}

/// Size of one linear-memory page in bytes.
pub const PAGE_SIZE: usize = 65536;
/// Maximum number of pages addressable by a 32-bit memory.
pub const MAX_PAGES: u32 = 65536;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_matching_follows_spec() {
        let l = |min, max| Limits { min, max };
        assert!(l(2, Some(3)).matches(&l(1, Some(4))));
        assert!(l(2, None).matches(&l(1, None)));
        assert!(!l(2, None).matches(&l(1, Some(4))));
        assert!(!l(0, Some(1)).matches(&l(1, None)));
        assert!(!l(1, Some(5)).matches(&l(1, Some(4))));
    }

    #[test]
    fn display_formats() {
        let ft = FuncType::new([ValType::I32, ValType::FUNCREF], [ValType::F64]);
        assert_eq!(ft.to_string(), "[i32 funcref] -> [f64]");
        assert_eq!(ExternKind::Memory.to_string(), "memory");
        assert!(ValType::I64.is_num());
        assert!(!ValType::EXTERNREF.is_num());
    }
}
