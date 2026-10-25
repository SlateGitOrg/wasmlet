//! A bounds-checked cursor over a byte slice with LEB128 decoding.
//!
//! Every read returns a [`DecodeError`] instead of panicking, which is what
//! lets the decoder be fuzzed for panics.

use crate::error::DecodeError;

/// Cursor over a byte slice. `base` is the absolute offset of `bytes[0]` in
/// the original module, so errors always report module-relative offsets.
#[derive(Debug, Clone)]
pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
    base: usize,
}

pub(crate) type DResult<T> = Result<T, DecodeError>;

impl<'a> Reader<'a> {
    pub(crate) fn new(bytes: &'a [u8], base: usize) -> Self {
        Reader { bytes, pos: 0, base }
    }

    /// Absolute offset of the next unread byte.
    pub(crate) fn offset(&self) -> usize {
        self.base + self.pos
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    pub(crate) fn remaining(&self) -> usize {
        self.bytes.len() - self.pos
    }

    pub(crate) fn err<T>(&self, message: impl Into<String>) -> DResult<T> {
        Err(DecodeError::new(self.offset(), message))
    }

    pub(crate) fn u8(&mut self) -> DResult<u8> {
        match self.bytes.get(self.pos) {
            Some(&b) => {
                self.pos += 1;
                Ok(b)
            }
            None => self.err("unexpected end"),
        }
    }

    pub(crate) fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    pub(crate) fn bytes(&mut self, n: usize) -> DResult<&'a [u8]> {
        if n > self.remaining() {
            return self.err("unexpected end");
        }
        let s = &self.bytes[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    /// Splits off a sub-reader of `n` bytes (used for sections and bodies).
    pub(crate) fn sub(&mut self, n: usize) -> DResult<Reader<'a>> {
        let base = self.offset();
        let bytes = self.bytes(n)?;
        Ok(Reader::new(bytes, base))
    }

    fn unsigned(&mut self, bits: u32) -> DResult<u64> {
        let mut result: u64 = 0;
        let mut shift: u32 = 0;
        loop {
            let b = self.u8()?;
            let payload = u64::from(b & 0x7f);
            if shift + 7 >= bits {
                // Final permitted byte: no continuation, no bits beyond `bits`.
                if b & 0x80 != 0 {
                    return self.err("integer representation too long");
                }
                let valid = bits - shift;
                if valid < 7 && payload >> valid != 0 {
                    return self.err("integer too large");
                }
                return Ok(result | (payload << shift));
            }
            result |= payload << shift;
            if b & 0x80 == 0 {
                return Ok(result);
            }
            shift += 7;
        }
    }

    fn signed(&mut self, bits: u32) -> DResult<i64> {
        let mut result: i64 = 0;
        let mut shift: u32 = 0;
        loop {
            let b = self.u8()?;
            let payload = i64::from(b & 0x7f);
            if shift + 7 >= bits {
                if b & 0x80 != 0 {
                    return self.err("integer representation too long");
                }
                let valid = bits - shift;
                if valid < 7 {
                    // Bits at and above the sign bit must all agree.
                    let upper = payload >> (valid - 1);
                    if upper != 0 && upper != (0x7f >> (valid - 1)) {
                        return self.err("integer too large");
                    }
                }
                result |= payload << shift;
                let total = shift + 7;
                if total < 64 {
                    // Sign-extend from the top payload bit.
                    result = (result << (64 - total)) >> (64 - total);
                }
                return Ok(result);
            }
            result |= payload << shift;
            shift += 7;
            if b & 0x80 == 0 {
                if b & 0x40 != 0 {
                    result |= -1i64 << shift;
                }
                return Ok(result);
            }
        }
    }

    pub(crate) fn u32(&mut self) -> DResult<u32> {
        // Truncation is lossless: `unsigned(32)` rejects anything wider.
        self.unsigned(32).map(|v| v as u32)
    }

    pub(crate) fn s32(&mut self) -> DResult<i32> {
        self.signed(32).map(|v| v as i32)
    }

    pub(crate) fn s33(&mut self) -> DResult<i64> {
        self.signed(33)
    }

    pub(crate) fn s64(&mut self) -> DResult<i64> {
        self.signed(64)
    }

    pub(crate) fn f32_bits(&mut self) -> DResult<u32> {
        let b = self.bytes(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub(crate) fn f64_bits(&mut self) -> DResult<u64> {
        let b = self.bytes(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(b);
        Ok(u64::from_le_bytes(a))
    }

    /// A vector length, sanity-checked against the remaining input so that a
    /// hostile count cannot trigger a huge allocation. Every element needs at
    /// least `min_elem_size` bytes.
    pub(crate) fn count(&mut self, min_elem_size: usize) -> DResult<u32> {
        let n = self.u32()?;
        if (n as usize).saturating_mul(min_elem_size) > self.remaining() {
            return self.err("length out of bounds");
        }
        Ok(n)
    }

    pub(crate) fn name(&mut self) -> DResult<String> {
        let len = self.u32()? as usize;
        if len > self.remaining() {
            return self.err("length out of bounds");
        }
        let start = self.offset();
        let raw = self.bytes(len)?;
        match std::str::from_utf8(raw) {
            Ok(s) => Ok(s.to_owned()),
            Err(_) => Err(DecodeError::new(start, "malformed UTF-8 encoding")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(b: &[u8]) -> Reader<'_> {
        Reader::new(b, 0)
    }

    #[test]
    fn unsigned_leb() {
        assert_eq!(r(&[0x00]).u32().unwrap(), 0);
        assert_eq!(r(&[0xe5, 0x8e, 0x26]).u32().unwrap(), 624485);
        assert_eq!(r(&[0xff, 0xff, 0xff, 0xff, 0x0f]).u32().unwrap(), u32::MAX);
        // Redundant zero padding is allowed up to the max length.
        assert_eq!(r(&[0x80, 0x80, 0x80, 0x80, 0x00]).u32().unwrap(), 0);
        let e = r(&[0xff, 0xff, 0xff, 0xff, 0x1f]).u32().unwrap_err();
        assert_eq!(e.message, "integer too large");
        let e = r(&[0x80, 0x80, 0x80, 0x80, 0x80, 0x00]).u32().unwrap_err();
        assert_eq!(e.message, "integer representation too long");
        let e = r(&[0x80]).u32().unwrap_err();
        assert_eq!(e.message, "unexpected end");
    }

    #[test]
    fn signed_leb() {
        assert_eq!(r(&[0x7f]).s32().unwrap(), -1);
        assert_eq!(r(&[0x3f]).s32().unwrap(), 63);
        assert_eq!(r(&[0x40]).s32().unwrap(), -64);
        assert_eq!(r(&[0x80, 0x7f]).s32().unwrap(), -128);
        assert_eq!(r(&[0xff, 0xff, 0xff, 0xff, 0x07]).s32().unwrap(), i32::MAX);
        assert_eq!(r(&[0x80, 0x80, 0x80, 0x80, 0x78]).s32().unwrap(), i32::MIN);
        assert_eq!(r(&[0xff, 0xff, 0xff, 0xff, 0x7f]).s32().unwrap(), -1);
        assert_eq!(
            r(&[0xff, 0xff, 0xff, 0xff, 0x0f]).s32().unwrap_err().message,
            "integer too large"
        );
        assert_eq!(
            r(&[0x80, 0x80, 0x80, 0x80, 0x70]).s32().unwrap_err().message,
            "integer too large"
        );
        let mut max64 = vec![0xff; 9];
        max64.push(0x00);
        assert_eq!(r(&max64).s64().unwrap(), i64::MAX);
        let mut min64 = vec![0x80; 9];
        min64.push(0x7f);
        assert_eq!(r(&min64).s64().unwrap(), i64::MIN);
        let mut bad = vec![0x80; 9];
        bad.push(0x01);
        assert_eq!(r(&bad).s64().unwrap_err().message, "integer too large");
        assert_eq!(r(&[0x40]).s33().unwrap(), -64);
        assert_eq!(r(&[0xff, 0xff, 0xff, 0xff, 0x0f]).s33().unwrap(), u32::MAX as i64);
    }

    #[test]
    fn floats_names_and_counts() {
        assert_eq!(r(&[0, 0, 0x80, 0x3f]).f32_bits().unwrap(), 1.0f32.to_bits());
        assert_eq!(r(&1.5f64.to_le_bytes()).f64_bits().unwrap(), 1.5f64.to_bits());
        assert_eq!(r(&[2, b'h', b'i']).name().unwrap(), "hi");
        assert_eq!(
            r(&[2, 0xff, 0xfe]).name().unwrap_err().message,
            "malformed UTF-8 encoding"
        );
        assert_eq!(r(&[9, 1]).name().unwrap_err().message, "length out of bounds");
        assert_eq!(r(&[200, 1]).count(1).unwrap_err().message, "length out of bounds");
        let mut rd = Reader::new(&[1, 2, 3, 4], 10);
        let mut s = rd.sub(2).unwrap();
        assert_eq!(s.offset(), 10);
        assert_eq!(s.u8().unwrap(), 1);
        assert_eq!(rd.offset(), 12);
        assert_eq!(rd.peek(), Some(3));
        assert!(rd.sub(5).is_err());
    }
}
