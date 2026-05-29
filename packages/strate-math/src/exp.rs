//! `exp_wad`: e^x for fixed-point `x` in WAD.
//!
//! ## Algorithm
//!
//! Range reduction: write `x = k * ln(2) + r` with `|r| <= ln(2)/2`,
//! then `exp(x) = 2^k * exp(r)`. The remainder `exp(r)` is computed with
//! an 8-term Taylor series, which gives ~17 decimal digits of accuracy
//! over `|r| <= ln(2)/2 ≈ 0.347` — well inside WAD precision.
//!
//! ## Domain
//!
//! - Upper bound: `x <= 88 * WAD`. Beyond this, `2^k` would overflow
//!   i128 even before multiplying by the Taylor result.
//! - Lower bound: `x >= -41 * WAD`. Below this, the result rounds to
//!   zero in WAD precision (and the algorithm would underflow `2^k`
//!   shifts).
//!
//! Out-of-domain inputs return [`crate::Error::ExpOutOfDomain`] from
//! [`exp_wad_checked`]; the unchecked [`exp_wad`] saturates (clamping
//! the upper end to `i128::MAX` and the lower end to `0`).
//!
//! ## Reference
//!
//! Adapted from Pendle's `PMath.sol::exp` and PRBMath's UD60x18 `exp`,
//! both of which derive from the standard "range-reduce + Taylor" trick
//! described in Cody & Waite (1980) and Muller (2016, "Elementary
//! Functions: Algorithms and Implementation").

use crate::{fixed::mul_wad_down_checked, Error, LN2, LN2_HALF, WAD};

/// Upper bound on `x` for `exp_wad`. `e^88 ~ 1.65e38` is just under
/// `i128::MAX ~ 1.7e38` even after the WAD scaling.
pub const EXP_MAX_INPUT: i128 = 88 * WAD;

/// Lower bound. `e^-41 ~ 1.5e-18`, below the WAD resolution.
pub const EXP_MIN_INPUT: i128 = -41 * WAD;

/// Compute `e^x` in WAD, saturating on out-of-range inputs.
///
/// - `x >= EXP_MAX_INPUT`  -> returns `i128::MAX`
/// - `x <= EXP_MIN_INPUT`  -> returns `0`
///
/// In normal contract code, prefer [`exp_wad_checked`] so caller can
/// translate the error into a protocol-level error rather than relying
/// on saturation. The saturating wrapper exists for the AMM curve
/// where the input has already been bounded by `lnImpliedRate` ranges.
#[must_use]
pub fn exp_wad(x: i128) -> i128 {
    match exp_wad_checked(x) {
        Ok(v) => v,
        Err(_) if x > 0 => i128::MAX,
        Err(_) => 0,
    }
}

/// Checked `e^x` in WAD.
///
/// Returns [`Error::ExpOutOfDomain`] if `x` is outside
/// `[EXP_MIN_INPUT, EXP_MAX_INPUT]`.
pub fn exp_wad_checked(x: i128) -> Result<i128, Error> {
    if x > EXP_MAX_INPUT {
        return Err(Error::ExpOutOfDomain);
    }
    if x < EXP_MIN_INPUT {
        return Err(Error::ExpOutOfDomain);
    }
    if x == 0 {
        return Ok(WAD);
    }

    // Step 1: range-reduce. Find k such that |x - k*LN2| <= LN2/2.
    // k = round(x / LN2). Use a half-even-ish round via add LN2_HALF
    // toward x's sign before truncating.
    let k_num = if x >= 0 { x + LN2_HALF } else { x - LN2_HALF };
    let k: i32 = (k_num / LN2) as i32;
    // r in (-LN2/2, LN2/2].
    let r: i128 = x - (i128::from(k)) * LN2;

    // Step 2: Taylor series for exp(r) at r=0:
    //   exp(r) = sum_{n=0..N} r^n / n!
    // We compute term-by-term, keeping each partial product in WAD.
    //
    // We use 14 terms (n=0..=14). The first omitted term is
    // r^15/15! < (LN2/2)^15/15! < 0.347^15 / 1.31e12 ≈ 7.4e-19 — below
    // WAD precision. After the 2^k shift the residual grows as 2^k,
    // so at the top of the domain (k≈127) the residual is ~1e20 wei
    // out of a value ~1e56 wei — relative error 1e-36, fine.
    //
    // We use `mul_wad_down` for term-wise rounding so successive terms
    // never inflate the partial sum.

    // Term 0: 1 (in WAD).
    let mut sum: i128 = WAD;
    // Term 1: r.
    let mut term: i128 = r;
    sum += term;
    // Term n: term_{n+1} = mul_wad(term_n, r) / (n+1).
    for n in 2i128..=14 {
        term = mul_wad_down_checked(term, r)? / n;
        sum += term;
    }
    // Guard against pathological negative sums (shouldn't happen for
    // |r| <= LN2/2 but defends against future domain widening).
    if sum <= 0 {
        return Ok(0);
    }

    // Step 3: multiply by 2^k. For positive k, shift left; for
    // negative, shift right. Both can be done as exact integer ops
    // on the WAD value because 2^k is exact.
    if k >= 0 {
        // Check overflow before shifting.
        let k_u: u32 = k as u32;
        if k_u >= 127 {
            return Err(Error::Overflow);
        }
        // sum * 2^k must fit in i128.
        let max_allowed = i128::MAX >> k_u;
        if sum > max_allowed {
            return Err(Error::Overflow);
        }
        Ok(sum << k_u)
    } else {
        let shift = (-k) as u32;
        if shift >= 127 {
            return Ok(0);
        }
        Ok(sum >> shift)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: assert two WAD values are within `tol` wei of each other.
    fn approx_eq(a: i128, b: i128, tol: i128) {
        let diff = (a - b).abs();
        assert!(
            diff <= tol,
            "approx_eq failed: a={}, b={}, diff={}, tol={}",
            a,
            b,
            diff,
            tol
        );
    }

    #[test]
    fn exp_zero() {
        assert_eq!(exp_wad(0), WAD);
    }

    #[test]
    fn exp_one() {
        // e = 2.718281828459045235360287471352662497...
        let e_wad: i128 = 2_718_281_828_459_045_235;
        // Our 8-term Taylor + range reduction should be within a few
        // hundred wei (relative error ~1e-18 from rounding) — but the
        // Taylor truncation at the upper end of the range-reduced
        // remainder is the dominant error. Allow 2000 wei (~7e-19
        // relative).
        approx_eq(exp_wad(WAD), e_wad, 2000);
    }

    #[test]
    fn exp_negative_one() {
        // 1/e = 0.367879441171442321595523770161460867...
        let inv_e_wad: i128 = 367_879_441_171_442_321;
        approx_eq(exp_wad(-WAD), inv_e_wad, 2000);
    }

    #[test]
    fn exp_ln2() {
        // exp(ln 2) = 2.
        approx_eq(exp_wad(LN2), 2 * WAD, 100);
    }

    #[test]
    fn exp_saturates_on_overflow() {
        assert_eq!(exp_wad(100 * WAD), i128::MAX);
    }

    #[test]
    fn exp_saturates_on_underflow() {
        assert_eq!(exp_wad(-50 * WAD), 0);
    }

    #[test]
    fn exp_checked_errors_on_out_of_domain() {
        assert_eq!(exp_wad_checked(100 * WAD), Err(Error::ExpOutOfDomain));
        assert_eq!(exp_wad_checked(-50 * WAD), Err(Error::ExpOutOfDomain));
    }
}
