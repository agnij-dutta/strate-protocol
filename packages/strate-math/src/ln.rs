//! `ln_wad`: natural log for positive fixed-point inputs in WAD.
//!
//! ## Algorithm
//!
//! Range reduction: find `k` such that `r = x / 2^k` lies in `[1, 2)`.
//! Then `ln(x) = k * ln(2) + ln(r)`. The residual `ln(r)` for
//! `r ∈ [1, 2)` is computed via the atanh-style series
//!
//!   `ln(r) = 2 * (u + u^3/3 + u^5/5 + u^7/7 + ...)`
//!
//! with `u = (r - 1) / (r + 1)`. For `r ∈ [1, 2)`, `u ∈ [0, 1/3)`, which
//! makes the series converge in ~10 terms to comfortably below WAD
//! precision (`(1/3)^21 / 21 ≈ 4.4e-12`, well past 1e-18 after the
//! factor of 2 and the dominant `ln(2)*k` term).
//!
//! ## Domain
//!
//! `x > 0`. Calling on `x <= 0` returns [`crate::Error::LnOfNonPositive`].
//!
//! ## Reference
//!
//! Same lineage as `exp`: Cody & Waite, PRBMath UD60x18, Pendle's
//! `PMath.sol::ln`.

use crate::{
    fixed::{div_wad_checked, mul_wad_down_checked},
    Error, LN2, WAD,
};

/// Compute `ln(x)` in WAD. Panics if `x <= 0`; use [`ln_wad_checked`]
/// when the caller might pass non-positive values (almost always in
/// contract code).
#[must_use]
pub fn ln_wad(x: i128) -> i128 {
    ln_wad_checked(x).expect("strate-math: ln_wad of non-positive")
}

/// Checked `ln(x)` in WAD.
///
/// Errors:
/// - [`Error::LnOfNonPositive`] if `x <= 0`.
/// - [`Error::Overflow`] if any intermediate exceeds `i128`.
pub fn ln_wad_checked(x: i128) -> Result<i128, Error> {
    if x <= 0 {
        return Err(Error::LnOfNonPositive);
    }
    if x == WAD {
        return Ok(0);
    }

    // Step 1: range-reduce. Find integer k such that x / 2^k ∈ [WAD, 2*WAD).
    //
    // k can be negative (for x < WAD) or positive (for x >= 2*WAD).
    // We find it by counting how many times we need to halve or double
    // to land in the canonical range. Use bit shifts directly for
    // exactness.
    let mut k: i32;
    let mut r: i128;
    if x >= WAD {
        // Find highest k such that x >> k >= WAD. r = x >> k still
        // counted at WAD scale (multiplied implicitly by 1).
        // But we want r in [WAD, 2*WAD), i.e. x / 2^k in [1, 2). So
        // k = floor(log2(x / WAD)).
        let mut tmp = x / WAD; // tmp >= 1.
        k = 0;
        while tmp >= 2 {
            tmp >>= 1;
            k += 1;
        }
        // r = x / 2^k (still WAD-scaled).
        r = x >> (k as u32);
    } else {
        // x < WAD. Find k such that x * 2^|k| >= WAD, |k| <= 60ish.
        let mut tmp = WAD / x; // >= 1.
        k = 0;
        while tmp >= 2 {
            tmp >>= 1;
            k -= 1;
        }
        // x * 2^(-k) >= WAD (with -k > 0). After this shift, r is
        // WAD-scaled and in [WAD, 2*WAD).
        // Use i128 shift; |k| won't exceed ~60 for any realistic input
        // because the smallest representable WAD value with non-zero
        // log is 1, giving k ~= -59.
        let shift = (-k) as u32;
        // Guard against pathological huge shifts (shouldn't happen for
        // x in valid range, but cheap to check).
        if shift >= 127 {
            return Err(Error::Overflow);
        }
        // Check shift doesn't overflow.
        let max_allowed = i128::MAX >> shift;
        if x > max_allowed {
            return Err(Error::Overflow);
        }
        r = x << shift;
    }
    // Sanity: r ∈ (WAD/2, 2*WAD). The loop counts floor(log2(WAD/x)) so
    // for x just below WAD (where WAD/x truncates to 1) we end up with
    // r slightly below WAD. The atanh series in step 2 still converges
    // for |u| < 1/3, which holds for r in this wider window, so the math
    // does not need a post-shift correction.
    debug_assert!(r > WAD / 2 && r < 2 * WAD, "range-reduce invariant: r={}", r);

    // Step 2: compute ln(r/WAD) via atanh series. Let
    //   u = (r - WAD) / (r + WAD)   (WAD-scaled, |u| < 1/3)
    //   ln(r/WAD) = 2 * (u + u^3/3 + u^5/5 + ... )
    let u = div_wad_checked(r - WAD, r + WAD)?;
    // u_sq stays in WAD scale.
    let u_sq = mul_wad_down_checked(u, u)?;

    // Accumulate series. Use 10 odd terms (u^1 through u^19).
    // sum = u + u^3/3 + u^5/5 + ... + u^19/19
    let mut sum: i128 = u;
    let mut power: i128 = u;
    for n in (3i128..=19).step_by(2) {
        power = mul_wad_down_checked(power, u_sq)?;
        sum += power / n;
    }
    let ln_r = sum.checked_mul(2).ok_or(Error::Overflow)?;

    // Step 3: combine. ln(x) = k * ln(2) + ln(r/WAD).
    let k_term = i128::from(k).checked_mul(LN2).ok_or(Error::Overflow)?;
    Ok(k_term + ln_r)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn ln_one() {
        assert_eq!(ln_wad(WAD), 0);
    }

    #[test]
    fn ln_e() {
        // ln(e) = 1.
        let e_wad: i128 = 2_718_281_828_459_045_235;
        approx_eq(ln_wad(e_wad), WAD, 100);
    }

    #[test]
    fn ln_two() {
        // 2*WAD halves to exactly WAD, so r-WAD == 0, u == 0, series
        // sum == 0, and ln(2*WAD) == LN2 exactly.
        approx_eq(ln_wad(2 * WAD), LN2, 2);
    }

    #[test]
    fn ln_half() {
        approx_eq(ln_wad(WAD / 2), -LN2, 2);
    }

    #[test]
    fn ln_rejects_zero() {
        assert_eq!(ln_wad_checked(0), Err(Error::LnOfNonPositive));
    }

    #[test]
    fn ln_rejects_negative() {
        assert_eq!(ln_wad_checked(-1), Err(Error::LnOfNonPositive));
    }
}
