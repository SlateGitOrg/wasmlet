//! Runtime traps.

use std::fmt;

/// An execution failure. The `Display` text matches the messages used by the
/// official spec test suite (`assert_trap`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trap {
    /// `unreachable` was executed.
    Unreachable,
    /// A memory access (or bulk-memory operation) was out of bounds.
    MemoryOutOfBounds,
    /// A table access (or bulk-table operation) was out of bounds.
    TableOutOfBounds,
    /// `call_indirect` index beyond the table size.
    UndefinedElement,
    /// `call_indirect` hit a null table entry.
    UninitializedElement,
    /// `call_indirect` signature check failed.
    IndirectCallTypeMismatch,
    /// Integer division or remainder by zero.
    IntegerDivideByZero,
    /// Signed division overflow or float-to-int result out of range.
    IntegerOverflow,
    /// Float-to-int conversion of NaN.
    InvalidConversion,
    /// The call-depth or value-stack limit was hit.
    CallStackExhausted,
    /// The configured fuel budget ran out.
    OutOfFuel,
    /// A host function requested process exit (WASI `proc_exit`).
    Exit(i32),
    /// A host function failed.
    Host(String),
}

impl fmt::Display for Trap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Trap::Unreachable => f.write_str("unreachable"),
            Trap::MemoryOutOfBounds => f.write_str("out of bounds memory access"),
            Trap::TableOutOfBounds => f.write_str("out of bounds table access"),
            Trap::UndefinedElement => f.write_str("undefined element"),
            Trap::UninitializedElement => f.write_str("uninitialized element"),
            Trap::IndirectCallTypeMismatch => f.write_str("indirect call type mismatch"),
            Trap::IntegerDivideByZero => f.write_str("integer divide by zero"),
            Trap::IntegerOverflow => f.write_str("integer overflow"),
            Trap::InvalidConversion => f.write_str("invalid conversion to integer"),
            Trap::CallStackExhausted => f.write_str("call stack exhausted"),
            Trap::OutOfFuel => f.write_str("all fuel consumed"),
            Trap::Exit(code) => write!(f, "exit with code {code}"),
            Trap::Host(m) => write!(f, "host error: {m}"),
        }
    }
}

impl std::error::Error for Trap {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_match_spec_suite() {
        let cases = [
            (Trap::Unreachable, "unreachable"),
            (Trap::MemoryOutOfBounds, "out of bounds memory access"),
            (Trap::TableOutOfBounds, "out of bounds table access"),
            (Trap::UndefinedElement, "undefined element"),
            (Trap::UninitializedElement, "uninitialized element"),
            (Trap::IndirectCallTypeMismatch, "indirect call type mismatch"),
            (Trap::IntegerDivideByZero, "integer divide by zero"),
            (Trap::IntegerOverflow, "integer overflow"),
            (Trap::InvalidConversion, "invalid conversion to integer"),
            (Trap::CallStackExhausted, "call stack exhausted"),
            (Trap::OutOfFuel, "all fuel consumed"),
            (Trap::Exit(3), "exit with code 3"),
            (Trap::Host("boom".into()), "host error: boom"),
        ];
        for (t, m) in cases {
            assert_eq!(t.to_string(), m);
        }
    }
}
