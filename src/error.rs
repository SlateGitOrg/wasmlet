//! Error types for every stage of the pipeline.

use std::fmt;

use crate::runtime::Trap;

/// The binary was malformed (spec: "malformed").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeError {
    /// Byte offset in the input where decoding failed.
    pub offset: usize,
    /// Human-readable reason, phrased like the reference interpreter where possible.
    pub message: String,
}

impl DecodeError {
    pub(crate) fn new(offset: usize, message: impl Into<String>) -> Self {
        DecodeError {
            offset,
            message: message.into(),
        }
    }
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "malformed module at offset {:#x}: {}",
            self.offset, self.message
        )
    }
}

impl std::error::Error for DecodeError {}

/// The module was well-formed but failed validation (spec: "invalid").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    /// Index of the function body being validated, if any.
    pub func: Option<u32>,
    /// Index of the offending instruction inside that body, if any.
    pub instr: Option<usize>,
    /// Human-readable reason, phrased like the reference interpreter where possible.
    pub message: String,
}

impl ValidationError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        ValidationError {
            func: None,
            instr: None,
            message: message.into(),
        }
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid module")?;
        if let Some(func) = self.func {
            write!(f, " in function {func}")?;
        }
        if let Some(i) = self.instr {
            write!(f, " at instruction {i}")?;
        }
        write!(f, ": {}", self.message)
    }
}

impl std::error::Error for ValidationError {}

/// Imports could not be resolved (spec: "unlinkable").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkError {
    /// Human-readable reason.
    pub message: String,
}

impl fmt::Display for LinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "link error: {}", self.message)
    }
}

impl std::error::Error for LinkError {}

/// Any failure produced by the public API.
#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    /// Binary decoding failed.
    Decode(DecodeError),
    /// Validation failed.
    Validation(ValidationError),
    /// Import resolution failed.
    Link(LinkError),
    /// Execution trapped (including during instantiation).
    Trap(Trap),
    /// Misuse of the embedding API (wrong argument count, unknown export, ...).
    Usage(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Decode(e) => e.fmt(f),
            Error::Validation(e) => e.fmt(f),
            Error::Link(e) => e.fmt(f),
            Error::Trap(t) => write!(f, "trap: {t}"),
            Error::Usage(m) => write!(f, "usage error: {m}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<DecodeError> for Error {
    fn from(e: DecodeError) -> Self {
        Error::Decode(e)
    }
}

impl From<ValidationError> for Error {
    fn from(e: ValidationError) -> Self {
        Error::Validation(e)
    }
}

impl From<LinkError> for Error {
    fn from(e: LinkError) -> Self {
        Error::Link(e)
    }
}

impl From<Trap> for Error {
    fn from(t: Trap) -> Self {
        Error::Trap(t)
    }
}

/// Convenience alias.
pub type Result<T, E = Error> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_includes_context() {
        let d = DecodeError::new(0x10, "unexpected end");
        assert_eq!(
            d.to_string(),
            "malformed module at offset 0x10: unexpected end"
        );
        let mut v = ValidationError::new("type mismatch");
        v.func = Some(3);
        v.instr = Some(7);
        assert_eq!(
            v.to_string(),
            "invalid module in function 3 at instruction 7: type mismatch"
        );
        let e: Error = LinkError {
            message: "unknown import".into(),
        }
        .into();
        assert_eq!(e.to_string(), "link error: unknown import");
        let e: Error = Trap::Unreachable.into();
        assert_eq!(e.to_string(), "trap: unreachable");
        assert_eq!(Error::Usage("x".into()).to_string(), "usage error: x");
        assert!(matches!(Error::from(v), Error::Validation(_)));
        assert!(matches!(Error::from(d), Error::Decode(_)));
    }
}
