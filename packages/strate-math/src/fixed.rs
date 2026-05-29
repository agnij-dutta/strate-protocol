//! Fixed-point WAD math (18-decimal scale on `i128`).
//!
//! ## Overflow strategy
//!
//! `a.checked_mul(b)` on `i128` only works when the product fits in 128
//! bits, which it usually doesn't (a WAD multiplication of two WAD
//! quantities reaches `~1e36`, well past `i128::MAX ≈ 1.7e38`, *and* the
//! intermediate product before dividing by WAD must be tracked at full
//! 256-bit precision to round correctly).
//!
//! We solve this by computing `|a| * |b|` as a 256-bit unsigned product
//! built from a four-way split of two `u128`s, then reducing it back by
//! a 256-by-128 division by the WAD scale or the explicit denominator.
//! Sign is reattached at the end.
//!
//! All public functions in this module are overflow-safe: they return
//! [`crate::Error::Overflow`] from their `_checked` variants when the
//! final result wouldn't fit in `i128`. The unchecked wrappers exposed
//! here panic on overflow with a deterministic message — they're
//! intended for internal use after the protocol layer has already
//! validated inputs.

use crate::{Error, WAD};

// -------------------------------------------------------------------------
// 256-bit unsigned scratch (high, low halves of a u256).
// -------------------------------------------------------------------------

/// Unsigned 256-bit integer as `(high_u128, low_u128)`.
///
/// Stored big-endian: value = (hi << 128) + lo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct U256 {
    hi: u128,
    lo: u128,
}

impl U256 {
    /// Additive identity. Used by the multiplication unit tests; kept on
    /// the type as part of its small numeric API surface.
    #[cfg_attr(not(test), allow(dead_code))]
    const ZERO: Self = Self { hi: 0, lo: 0 };

    /// Compute `a * b` as a 256-bit unsigned integer.
    ///
    /// Uses the schoolbook decomposition `(aH:aL) * (bH:bL)` where each
    /// half is 64 bits. The four 64x64 partial products fit in `u128`,
    /// and we sum them with explicit carry tracking.
    #[inline]
    fn mul_u128(a: u128, b: u128) -> Self {
        // Split each operand into two 64-bit halves.
        let a_lo: u128 = a & 0xFFFF_FFFF_FFFF_FFFF;
        let a_hi: u128 = a >> 64;
        let b_lo: u128 = b & 0xFFFF_FFFF_FFFF_FFFF;
        let b_hi: u128 = b >> 64;

        // Four 64x64 -> 128 partials.
        let ll = a_lo * b_lo;
        let lh = a_lo * b_hi;
        let hl = a_hi * b_lo;
        let hh = a_hi * b_hi;

        // Assemble. The 256-bit layout (most-significant first) is:
        //
        //     [hh.hi : hh.lo + lh.hi + hl.hi : lh.lo + hl.lo + ll.hi : ll.lo]
        //
        // Build it in u128 lanes with carry propagation.
        let mid = (ll >> 64) + (lh & 0xFFFF_FFFF_FFFF_FFFF) + (hl & 0xFFFF_FFFF_FFFF_FFFF);
        let lo = (ll & 0xFFFF_FFFF_FFFF_FFFF) | (mid << 64);

        // Carries out of the middle column.
        let mid_carry = mid >> 64;
        let hi = hh + (lh >> 64) + (hl >> 64) + mid_carry;

        Self { hi, lo }
    }

    /// Divide a 256-bit value by a non-zero `u128` divisor, returning
    /// `(quotient, remainder)`. The quotient must itself fit in
    /// `u128`; otherwise `Err(Error::Overflow)` is returned. This is
    /// always true in our call sites because we either divide by `WAD`
    /// (and the numerator is bounded by two WAD operands) or by a
    /// denominator strictly greater than the numerator's high half.
    ///
    /// Algorithm: restoring binary long division. The invariant is
    /// that after processing bit `i` (MSB-first), the partial
    /// remainder `r` satisfies `r < d` and the bits of the dividend
    /// from position `255` down to `255-i` have been consumed.
    #[inline]
    fn div_rem_u128(self, d: u128) -> Result<(u128, u128), Error> {
        debug_assert!(d != 0);
        // Fast path: hi == 0, native u128 division.
        if self.hi == 0 {
            return Ok((self.lo / d, self.lo % d));
        }
        // If hi >= d, the quotient won't fit into u128.
        if self.hi >= d {
            return Err(Error::Overflow);
        }

        // Restoring binary division.
        //
        // At each step we want to compute r' = 2*r + bit; if that
        // exceeds d, subtract d and emit a 1 quotient bit. The
        // subtlety is that `2*r + bit` can exceed `u128::MAX` when r's
        // top bit is set, so we track that overflow as a "phantom" bit
        // worth 2^128. We never actually need to store a 129-bit
        // value because we immediately subtract d (which is at most
        // u128::MAX) — the result fits in u128.
        let mut q: u128 = 0;
        let mut r: u128 = self.hi; // r < d already (checked above).
        let lo = self.lo;

        // Consume the 128 bits of `lo`, MSB first.
        let mut i: u32 = 128;
        while i > 0 {
            i -= 1;
            let bit = (lo >> i) & 1;
            // Shift r left by 1, capturing the lost top bit.
            let r_top = r >> 127;
            let r_shifted = (r << 1) | bit;
            // True value is r_shifted + (r_top << 128). Compare to d.
            // If r_top == 1, true value is in [2^128, 2^128 + u128::MAX]
            // which is always >= d, so subtract.
            let took = r_top == 1 || r_shifted >= d;
            r = if took {
                // r_shifted - d in mod 2^128. When r_top == 1, the
                // mathematical subtraction is (r_shifted + 2^128) - d,
                // which equals r_shifted.wrapping_sub(d) modulo 2^128.
                // That's the value we want as the new r, and it lies
                // in [0, d) because d <= 2^128 - 1.
                r_shifted.wrapping_sub(d)
            } else {
                r_shifted
            };
            q = (q << 1) | u128::from(took);
        }
        Ok((q, r))
    }
}

// -------------------------------------------------------------------------
// Sign-aware muldiv core.
// -------------------------------------------------------------------------

/// Compute `(a * b) / denom` with explicit rounding mode.
///
/// Returns `Err(Error::Overflow)` if the unsigned magnitude of the
/// quotient exceeds `i128::MAX + 1` (the asymmetric boundary of two's
/// complement), and `Err(Error::DivisionByZero)` if `denom == 0`.
fn muldiv_signed(a: i128, b: i128, denom: i128, mode: SignedRounding) -> Result<i128, Error> {
    if denom == 0 {
        return Err(Error::DivisionByZero);
    }
    // Fast paths preserve exact zero.
    if a == 0 || b == 0 {
        return Ok(0);
    }

    let neg = (a < 0) ^ (b < 0) ^ (denom < 0);
    let abs_a = unsigned_abs_i128(a);
    let abs_b = unsigned_abs_i128(b);
    let abs_d = unsigned_abs_i128(denom);

    let prod = U256::mul_u128(abs_a, abs_b);
    let (mut q, r) = prod.div_rem_u128(abs_d)?;

    // Apply rounding on the *magnitude*, then re-sign.
    let needs_bump = match mode {
        SignedRounding::TowardZero => false,
        SignedRounding::AwayFromZero => r != 0,
        SignedRounding::HalfEven => {
            // We need to decide if `r > d/2` (bump), `r < d/2` (don't),
            // or `r == d/2` (tie -> round to even). Since `r` and `d`
            // are u128 and `2*r` can overflow, do the comparison via
            // `half = d >> 1` and the parity of `d`:
            //
            // - d even: half = d/2 exactly. r > half -> bump; r ==
            //   half -> tie (round to even); r < half -> don't bump.
            // - d odd:  half = (d-1)/2; true threshold is half + 0.5.
            //   r > half -> r >= half+1 > half+0.5 -> bump.
            //   r == half -> r < half + 0.5 -> don't bump.
            //   r < half -> don't bump.
            //   No exact ties are possible for odd d with integer r.
            let half = abs_d >> 1;
            let d_is_even = (abs_d & 1) == 0;
            if r > half {
                true
            } else if r == half && d_is_even {
                // Exact tie. Round to even: bump iff q is odd.
                (q & 1) == 1
            } else {
                false
            }
        }
    };
    if needs_bump {
        q = q.checked_add(1).ok_or(Error::Overflow)?;
    }

    // Re-attach sign, respecting i128's asymmetric range.
    if neg {
        if q > (i128::MAX as u128) + 1 {
            return Err(Error::Overflow);
        }
        if q == (i128::MAX as u128) + 1 {
            return Ok(i128::MIN);
        }
        Ok(-(q as i128))
    } else {
        if q > i128::MAX as u128 {
            return Err(Error::Overflow);
        }
        Ok(q as i128)
    }
}

/// Internal rounding direction (sign-aware).
///
/// We translate the user-facing `Floor`/`Ceiling` into magnitude-based
/// `TowardZero`/`AwayFromZero` after extracting sign, because the actual
/// arithmetic happens on `u128`. Callers that care about *signed* floor
/// (i.e. `Floor(-0.5) = -1`) handle the sign flip themselves before
/// invoking this routine. In the math we ship today, all directional
/// rounding happens on non-negative magnitudes, so this is sound.
#[derive(Clone, Copy)]
enum SignedRounding {
    TowardZero,
    AwayFromZero,
    HalfEven,
}

/// `i128::abs` that handles `i128::MIN` without panicking.
#[inline]
fn unsigned_abs_i128(x: i128) -> u128 {
    if x == i128::MIN {
        1u128 << 127
    } else if x < 0 {
        (-x) as u128
    } else {
        x as u128
    }
}

// -------------------------------------------------------------------------
// Public WAD multiply / divide.
// -------------------------------------------------------------------------

/// `a * b / WAD` rounded **half to even** (banker's rounding).
///
/// Use for value-neutral conversions. Overflow-safe via 256-bit
/// intermediate. Panics if the final result wouldn't fit in `i128`; use
/// [`mul_wad_checked`] in protocol code paths where the inputs aren't
/// pre-validated.
#[must_use]
pub fn mul_wad(a: i128, b: i128) -> i128 {
    mul_wad_checked(a, b).expect("strate-math: mul_wad overflow")
}

/// `a * b / WAD` rounded **away from zero**.
///
/// On non-negative operands (the common case for fees and debt) this
/// is equivalent to rounding toward `+inf`. On mixed-sign operands it
/// rounds the magnitude up — i.e. it always returns at least as
/// large an absolute value as the exact result.
///
/// Use for **fees and debt** owed to the protocol — over-charging the
/// user by a wei is always safer than under-charging.
#[must_use]
pub fn mul_wad_up(a: i128, b: i128) -> i128 {
    mul_wad_up_checked(a, b).expect("strate-math: mul_wad_up overflow")
}

/// `a * b / WAD` rounded **toward zero**.
///
/// On non-negative operands this is equivalent to rounding toward
/// `-inf`. Use for **assets out** to the user — under-paying by a
/// wei is always safer than over-paying.
#[must_use]
pub fn mul_wad_down(a: i128, b: i128) -> i128 {
    mul_wad_down_checked(a, b).expect("strate-math: mul_wad_down overflow")
}

/// `a * WAD / b` rounded **half to even**.
#[must_use]
pub fn div_wad(a: i128, b: i128) -> i128 {
    div_wad_checked(a, b).expect("strate-math: div_wad failure")
}

/// `a * WAD / b` rounded **away from zero** (use for liabilities/fees).
#[must_use]
pub fn div_wad_up(a: i128, b: i128) -> i128 {
    div_wad_up_checked(a, b).expect("strate-math: div_wad_up failure")
}

/// `a * WAD / b` rounded **toward zero** (use for assets out).
#[must_use]
pub fn div_wad_down(a: i128, b: i128) -> i128 {
    div_wad_down_checked(a, b).expect("strate-math: div_wad_down failure")
}

/// `(a * b) / denom` rounded **toward zero**.
///
/// Named `floor` for parity with the Pendle reference; on non-negative
/// inputs this is mathematically the floor.
#[must_use]
pub fn muldiv_floor(a: i128, b: i128, denom: i128) -> i128 {
    muldiv_floor_checked(a, b, denom).expect("strate-math: muldiv_floor failure")
}

/// `(a * b) / denom` rounded **away from zero**.
///
/// Named `ceil` for parity with the Pendle reference; on non-negative
/// inputs this is mathematically the ceiling.
#[must_use]
pub fn muldiv_ceil(a: i128, b: i128, denom: i128) -> i128 {
    muldiv_ceil_checked(a, b, denom).expect("strate-math: muldiv_ceil failure")
}

// -------------------------------------------------------------------------
// Checked variants — preferred in contract code.
// -------------------------------------------------------------------------

/// Checked half-to-even multiply.
pub fn mul_wad_checked(a: i128, b: i128) -> Result<i128, Error> {
    muldiv_signed(a, b, WAD, SignedRounding::HalfEven)
}

/// Checked round-up multiply (signed magnitudes).
pub fn mul_wad_up_checked(a: i128, b: i128) -> Result<i128, Error> {
    // "Round up" in the fee/debt sense means *away from zero* when both
    // operands have the same sign (positive). For mixed-sign inputs we
    // still round magnitudes away from zero so that the absolute value
    // is at least as large as the exact result — which is what fee logic
    // wants.
    muldiv_signed(a, b, WAD, SignedRounding::AwayFromZero)
}

/// Checked round-down multiply.
pub fn mul_wad_down_checked(a: i128, b: i128) -> Result<i128, Error> {
    muldiv_signed(a, b, WAD, SignedRounding::TowardZero)
}

/// Checked half-to-even divide.
pub fn div_wad_checked(a: i128, b: i128) -> Result<i128, Error> {
    muldiv_signed(a, WAD, b, SignedRounding::HalfEven)
}

/// Checked round-up divide.
pub fn div_wad_up_checked(a: i128, b: i128) -> Result<i128, Error> {
    muldiv_signed(a, WAD, b, SignedRounding::AwayFromZero)
}

/// Checked round-down divide.
pub fn div_wad_down_checked(a: i128, b: i128) -> Result<i128, Error> {
    muldiv_signed(a, WAD, b, SignedRounding::TowardZero)
}

/// Checked floor muldiv.
pub fn muldiv_floor_checked(a: i128, b: i128, denom: i128) -> Result<i128, Error> {
    muldiv_signed(a, b, denom, SignedRounding::TowardZero)
}

/// Checked ceil muldiv.
pub fn muldiv_ceil_checked(a: i128, b: i128, denom: i128) -> Result<i128, Error> {
    muldiv_signed(a, b, denom, SignedRounding::AwayFromZero)
}

/// Checked muldiv with an explicit [`crate::RoundingMode`].
///
/// Use this when the rounding direction is data-dependent (rare). In
/// most contract code, prefer the named `_up` / `_down` variants
/// because they're easier to audit.
pub fn muldiv_checked(
    a: i128,
    b: i128,
    denom: i128,
    mode: crate::RoundingMode,
) -> Result<i128, Error> {
    let signed = match mode {
        crate::RoundingMode::Floor => SignedRounding::TowardZero,
        crate::RoundingMode::Ceiling => SignedRounding::AwayFromZero,
        crate::RoundingMode::HalfEven => SignedRounding::HalfEven,
    };
    muldiv_signed(a, b, denom, signed)
}

// -------------------------------------------------------------------------
// Tests (in-module unit tests for the U256 scratch; vector tests live
// in tests/math_vectors.rs).
// -------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn u256_mul_basic() {
        let p = U256::mul_u128(0, 0);
        assert_eq!(p, U256::ZERO);

        let p = U256::mul_u128(1, 1);
        assert_eq!(p, U256 { hi: 0, lo: 1 });

        // (2^64) * (2^64) = 2^128 -> (hi=1, lo=0)
        let p = U256::mul_u128(1u128 << 64, 1u128 << 64);
        assert_eq!(p, U256 { hi: 1, lo: 0 });
    }

    #[test]
    fn u256_mul_max() {
        // u128::MAX * u128::MAX = 2^256 - 2^129 + 1
        let p = U256::mul_u128(u128::MAX, u128::MAX);
        assert_eq!(p.hi, u128::MAX - 1);
        assert_eq!(p.lo, 1);
    }

    #[test]
    fn u256_div_basic() {
        let p = U256 { hi: 0, lo: 100 };
        let (q, r) = p.div_rem_u128(7).unwrap();
        assert_eq!(q, 14);
        assert_eq!(r, 2);
    }

    #[test]
    fn u256_div_overflow() {
        // hi >= d means quotient doesn't fit in u128.
        let p = U256 { hi: 5, lo: 0 };
        assert_eq!(p.div_rem_u128(3), Err(Error::Overflow));
    }

    #[test]
    fn u256_div_max_round_trip() {
        // (u128::MAX * 7) / 7 == u128::MAX
        let p = U256::mul_u128(u128::MAX, 7);
        let (q, r) = p.div_rem_u128(7).unwrap();
        assert_eq!(q, u128::MAX);
        assert_eq!(r, 0);
    }

    #[test]
    fn mul_wad_identity() {
        assert_eq!(mul_wad(WAD, WAD), WAD);
        assert_eq!(mul_wad(123_456_789, WAD), 123_456_789);
        assert_eq!(mul_wad(-WAD, WAD), -WAD);
    }

    #[test]
    fn div_by_zero() {
        assert_eq!(div_wad_checked(1, 0), Err(Error::DivisionByZero));
        assert_eq!(muldiv_floor_checked(1, 2, 0), Err(Error::DivisionByZero));
    }

    #[test]
    fn rounding_up_vs_down() {
        // 7e17 * 7e17 / 1e18 = 4.9e17 exact, no rounding gap.
        let a: i128 = 700_000_000_000_000_000;
        assert_eq!(mul_wad_down(a, a), 490_000_000_000_000_000);
        assert_eq!(mul_wad_up(a, a), 490_000_000_000_000_000);

        // Force a remainder: 1 * 1 / WAD = 0 floor, 1 ceil.
        assert_eq!(mul_wad_down(1, 1), 0);
        assert_eq!(mul_wad_up(1, 1), 1);
    }

    #[test]
    fn halfeven_tie_breaks_to_even() {
        // (5 * 1) / 10 = 0.5 exact tie. Half-even rounds to even -> 0.
        assert_eq!(
            muldiv_signed(5, 1, 10, SignedRounding::HalfEven).unwrap(),
            0
        );
        // (15 * 1) / 10 = 1.5 exact tie. Round to even -> 2.
        assert_eq!(
            muldiv_signed(15, 1, 10, SignedRounding::HalfEven).unwrap(),
            2
        );
        // (25 * 1) / 10 = 2.5 exact tie. Round to even -> 2.
        assert_eq!(
            muldiv_signed(25, 1, 10, SignedRounding::HalfEven).unwrap(),
            2
        );
    }

    #[test]
    fn signed_min_handled() {
        // i128::MIN has no positive counterpart; ensure unsigned_abs
        // produces 2^127 and downstream code doesn't panic.
        let a = i128::MIN;
        let b = WAD;
        let r = mul_wad_checked(a, b).unwrap();
        // a * WAD / WAD = a
        assert_eq!(r, a);
    }

    #[test]
    fn overflow_detected() {
        // i128::MAX * 2 / WAD: product is 2 * i128::MAX, divided by WAD
        // is roughly 3.4e20 — fits in i128 (1.7e38). So pick a worse
        // case: i128::MAX * i128::MAX / 1 = absurd overflow.
        let r = muldiv_ceil_checked(i128::MAX, i128::MAX, 1);
        assert_eq!(r, Err(Error::Overflow));
    }
}
