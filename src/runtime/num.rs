//! Numeric semantics that differ from Rust's defaults: float min/max, NaN-
//! and sign-exact bit operations, and checked float-to-int truncation.

use super::Trap;

pub(crate) fn fmin32(a: f32, b: f32) -> f32 {
    if a.is_nan() || b.is_nan() {
        // Any arithmetic on a NaN yields a quieted NaN, as the spec requires.
        return a + b;
    }
    if a == b {
        // Distinguishes -0 and +0: min(-0, +0) = -0.
        return f32::from_bits(a.to_bits() | b.to_bits());
    }
    a.min(b)
}

pub(crate) fn fmax32(a: f32, b: f32) -> f32 {
    if a.is_nan() || b.is_nan() {
        return a + b;
    }
    if a == b {
        return f32::from_bits(a.to_bits() & b.to_bits());
    }
    a.max(b)
}

pub(crate) fn fmin64(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        return a + b;
    }
    if a == b {
        return f64::from_bits(a.to_bits() | b.to_bits());
    }
    a.min(b)
}

pub(crate) fn fmax64(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        return a + b;
    }
    if a == b {
        return f64::from_bits(a.to_bits() & b.to_bits());
    }
    a.max(b)
}

const F32_SIGN: u32 = 0x8000_0000;
const F64_SIGN: u64 = 0x8000_0000_0000_0000;

pub(crate) fn fabs32(bits: u32) -> u32 {
    bits & !F32_SIGN
}
pub(crate) fn fneg32(bits: u32) -> u32 {
    bits ^ F32_SIGN
}
pub(crate) fn fcopysign32(a: u32, b: u32) -> u32 {
    (a & !F32_SIGN) | (b & F32_SIGN)
}
pub(crate) fn fabs64(bits: u64) -> u64 {
    bits & !F64_SIGN
}
pub(crate) fn fneg64(bits: u64) -> u64 {
    bits ^ F64_SIGN
}
pub(crate) fn fcopysign64(a: u64, b: u64) -> u64 {
    (a & !F64_SIGN) | (b & F64_SIGN)
}

/// Checked truncation: NaN is an invalid conversion; values whose truncation
/// falls outside `(lo, hi)` (exclusive bounds, given as exactly-representable
/// floats) overflow.
macro_rules! checked_trunc {
    ($name:ident, $f:ty, $i:ty, $lo:expr, $hi:expr) => {
        pub(crate) fn $name(x: $f) -> Result<$i, Trap> {
            if x.is_nan() {
                return Err(Trap::InvalidConversion);
            }
            if !(x > $lo && x < $hi) {
                return Err(Trap::IntegerOverflow);
            }
            Ok(x as $i)
        }
    };
}

// Bounds: the largest float strictly below the representable minimum minus
// one (exclusive), and 2^N (exclusive).
checked_trunc!(i32_trunc_f32_s, f32, i32, -2147483904.0f32, 2147483648.0f32);
checked_trunc!(i32_trunc_f32_u, f32, u32, -1.0f32, 4294967296.0f32);
checked_trunc!(i32_trunc_f64_s, f64, i32, -2147483649.0f64, 2147483648.0f64);
checked_trunc!(i32_trunc_f64_u, f64, u32, -1.0f64, 4294967296.0f64);
checked_trunc!(i64_trunc_f32_s, f32, i64, -9223373136366403584.0f32, 9223372036854775808.0f32);
checked_trunc!(i64_trunc_f32_u, f32, u64, -1.0f32, 18446744073709551616.0f32);
checked_trunc!(i64_trunc_f64_s, f64, i64, -9223372036854777856.0f64, 9223372036854775808.0f64);
checked_trunc!(i64_trunc_f64_u, f64, u64, -1.0f64, 18446744073709551616.0f64);

macro_rules! div_rem {
    ($div_s:ident, $div_u:ident, $rem_s:ident, $rem_u:ident, $s:ty, $u:ty) => {
        pub(crate) fn $div_s(a: $s, b: $s) -> Result<$s, Trap> {
            if b == 0 {
                return Err(Trap::IntegerDivideByZero);
            }
            if a == <$s>::MIN && b == -1 {
                return Err(Trap::IntegerOverflow);
            }
            Ok(a.wrapping_div(b))
        }
        pub(crate) fn $div_u(a: $u, b: $u) -> Result<$u, Trap> {
            if b == 0 {
                return Err(Trap::IntegerDivideByZero);
            }
            Ok(a / b)
        }
        pub(crate) fn $rem_s(a: $s, b: $s) -> Result<$s, Trap> {
            if b == 0 {
                return Err(Trap::IntegerDivideByZero);
            }
            Ok(a.wrapping_rem(b))
        }
        pub(crate) fn $rem_u(a: $u, b: $u) -> Result<$u, Trap> {
            if b == 0 {
                return Err(Trap::IntegerDivideByZero);
            }
            Ok(a % b)
        }
    };
}

div_rem!(i32_div_s, i32_div_u, i32_rem_s, i32_rem_u, i32, u32);
div_rem!(i64_div_s, i64_div_u, i64_rem_s, i64_rem_u, i64, u64);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn min_max_signed_zero_and_nan() {
        assert_eq!(fmin32(-0.0, 0.0).to_bits(), (-0.0f32).to_bits());
        assert_eq!(fmax32(-0.0, 0.0).to_bits(), 0.0f32.to_bits());
        assert_eq!(fmin64(0.0, -0.0).to_bits(), (-0.0f64).to_bits());
        assert_eq!(fmax64(0.0, -0.0).to_bits(), 0.0f64.to_bits());
        assert!(fmin32(f32::NAN, 1.0).is_nan());
        assert!(fmax32(1.0, f32::NAN).is_nan());
        assert!(fmin64(f64::NAN, 1.0).is_nan());
        assert!(fmax64(2.0, f64::NAN).is_nan());
        assert_eq!(fmin32(1.0, 2.0), 1.0);
        assert_eq!(fmax64(1.0, 2.0), 2.0);
        assert_eq!(fmax32(3.0, 2.0), 3.0);
        assert_eq!(fmin64(3.0, 2.0), 2.0);
        // Quiet bit is set on a propagated signalling NaN.
        let snan = f32::from_bits(0x7f80_0001);
        assert_ne!(fmin32(snan, 1.0).to_bits() & 0x0040_0000, 0);
    }

    #[test]
    fn sign_bit_ops_preserve_payload() {
        let nan = 0x7fc0_1234u32;
        assert_eq!(fneg32(nan), 0xffc0_1234);
        assert_eq!(fabs32(0xffc0_1234), nan);
        assert_eq!(fcopysign32(nan, 0x8000_0000), 0xffc0_1234);
        assert_eq!(fneg64(0), F64_SIGN);
        assert_eq!(fabs64(F64_SIGN | 5), 5);
        assert_eq!(fcopysign64(5, F64_SIGN), F64_SIGN | 5);
    }

    #[test]
    fn truncation_bounds() {
        assert_eq!(i32_trunc_f32_s(-2147483648.0), Ok(i32::MIN));
        assert_eq!(i32_trunc_f32_s(2147483648.0), Err(Trap::IntegerOverflow));
        assert_eq!(i32_trunc_f32_s(f32::NAN), Err(Trap::InvalidConversion));
        assert_eq!(i32_trunc_f64_s(-2147483648.9), Ok(i32::MIN));
        assert_eq!(i32_trunc_f64_s(-2147483649.0), Err(Trap::IntegerOverflow));
        assert_eq!(i32_trunc_f32_u(-0.9), Ok(0));
        assert_eq!(i32_trunc_f32_u(-1.0), Err(Trap::IntegerOverflow));
        assert_eq!(i32_trunc_f64_u(4294967295.9), Ok(u32::MAX));
        assert_eq!(i32_trunc_f64_u(4294967296.0), Err(Trap::IntegerOverflow));
        assert_eq!(i64_trunc_f32_s(-9223372036854775808.0), Ok(i64::MIN));
        assert_eq!(i64_trunc_f32_s(9223372036854775808.0), Err(Trap::IntegerOverflow));
        assert_eq!(i64_trunc_f64_s(-9223372036854775808.0), Ok(i64::MIN));
        assert_eq!(i64_trunc_f64_s(f64::INFINITY), Err(Trap::IntegerOverflow));
        assert_eq!(i64_trunc_f32_u(18446742974197923840.0), Ok(18446742974197923840));
        assert_eq!(i64_trunc_f64_u(-0.5), Ok(0));
        assert_eq!(i64_trunc_f64_u(f64::NAN), Err(Trap::InvalidConversion));
    }

    #[test]
    fn division() {
        assert_eq!(i32_div_s(i32::MIN, -1), Err(Trap::IntegerOverflow));
        assert_eq!(i32_div_s(7, 0), Err(Trap::IntegerDivideByZero));
        assert_eq!(i32_div_s(-7, 2), Ok(-3));
        assert_eq!(i32_rem_s(i32::MIN, -1), Ok(0));
        assert_eq!(i32_rem_s(-7, 2), Ok(-1));
        assert_eq!(i32_div_u(7, 2), Ok(3));
        assert_eq!(i32_rem_u(7, 0), Err(Trap::IntegerDivideByZero));
        assert_eq!(i64_div_u(7, 0), Err(Trap::IntegerDivideByZero));
        assert_eq!(i64_rem_u(7, 4), Ok(3));
        assert_eq!(i64_rem_s(1, 0), Err(Trap::IntegerDivideByZero));
        assert_eq!(i64_div_s(i64::MIN, -1), Err(Trap::IntegerOverflow));
    }
}
