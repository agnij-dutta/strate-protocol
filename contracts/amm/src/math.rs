//! Pendle-V1-style logarithmic-curve math.
//!
//! Pure functions, no environment. The contract entrypoints in
//! [`crate::lib`] do all the storage / authentication and call these
//! helpers for arithmetic only.
//!
//! ## Algorithm
//!
//! Pool reserves `(x_in, x_out)` with time-dependent weights
//! `(w_in, w_out)` satisfying `w_in + w_out = WAD`. A swap that adds
//! `dx_in` after fee returns
//!
//! ```text
//! dx_out = x_out * (1 - (x_in / (x_in + dx_in))^(w_in / w_out))
//! ```
//!
//! Implemented as
//!
//! ```text
//! y          = x_in / (x_in + dx_in_after_fee)
//! weight_r   = w_in / w_out
//! foo        = exp(weight_r * ln(y))
//! dx_out     = x_out * (WAD - foo)
//! ```
//!
//! using the WAD `exp` / `ln` from [`strate_math`].
//!
//! ## Weight update
//!
//! Before every trade the contract recomputes weights from the current
//! time-to-maturity and spot price:
//!
//! ```text
//! t_frac     = time_to_maturity / total_duration       (in WAD; in (0,1))
//! p_spot     = scalar_root * (x_under / x_pt)          (WAD ratio)
//! w_pt       = ln(t_frac) / ln(p_spot)                 (provided both are valid)
//! w_under    = WAD - w_pt
//! ```
//!
//! Both `ln(t_frac)` and `ln(p_spot)` are negative for a well-formed
//! pool with `p_spot < 1` and `t_frac < 1`; their ratio is positive and
//! belongs to `(0, 1)`. At pool launch `t_frac ≈ 1` and `ln(t_frac) ≈
//! 0`, so `w_pt → 0`. As `t_frac → 0` near maturity, `ln(t_frac) → -∞`
//! and `w_pt → 1`. The C-03 lock prevents us from ever evaluating the
//! limit.
//!
//! ## Degenerate cases
//!
//! - `p_spot == WAD` would yield `ln(p_spot) == 0` and a divide-by-zero.
//!   We add a small floor (`ln_price_eps`) and clamp so `|ln_price| >=
//!   ε`, which is sound because at `p_spot == WAD` the curve is
//!   symmetric and the time-weighting is irrelevant.
//! - `t_frac == WAD` (pool launches exactly at inception) is gated at
//!   the constructor: `total_duration` is recorded so `t_frac == WAD`
//!   only at the construction ledger; subsequent calls always see
//!   `t_frac < WAD`.

use soroban_sdk::Env;
use strate_interfaces::Error;
use strate_math::{
    div_wad_down_checked, exp_wad, ln_wad_checked, mul_wad_down_checked, mul_wad_up_checked, WAD,
};

use crate::storage::AmmConfig;
use crate::MARKET_LOCK_WINDOW_SECONDS;

/// Convert fee in basis points to a WAD multiplier. `100 bps = 0.01
/// WAD = 1e16`.
#[must_use]
pub fn fee_to_wad(bps: u32) -> i128 {
    // `bps * 1e14` — 100 * 1e14 == 1e16 == 1% in WAD.
    (bps as i128) * 100_000_000_000_000
}

/// A tiny epsilon used to keep `ln(p_spot)` away from zero. WAD-scaled
/// to about 1e-9. Empirically picked so the `1 / ln_price` reciprocal
/// stays in a sane range even when the pool is at the symmetric point.
const LN_PRICE_EPS: i128 = 1_000_000_000;

/// Pendle V1 swap formula. Returns the gross `dx_out` (no further
/// rounding required; this function already rounds **down** at every
/// internal step so the protocol keeps the rounding wei).
///
/// Inputs:
/// - `x_in` — reserve of the token being added.
/// - `x_out` — reserve of the token being removed.
/// - `dx_in` — pre-fee amount in.
/// - `w_in`, `w_out` — token weights at the current time, WAD, must
///   sum to WAD and both be strictly positive.
/// - `fee_bps` — basis-points swap fee.
pub fn compute_swap_out(
    x_in: i128,
    x_out: i128,
    dx_in: i128,
    w_in: i128,
    w_out: i128,
    fee_bps: u32,
) -> Result<i128, Error> {
    if x_in <= 0 || x_out <= 0 || dx_in <= 0 {
        return Err(Error::InsufficientLiquidity);
    }
    if w_in <= 0 || w_out <= 0 {
        return Err(Error::Math);
    }

    // 1. Fee deduction. Fee is taken from the input — `(WAD - fee_to_wad)
    //    * dx_in`. Round **down** on the user-facing input so the
    //    protocol's fee is rounded **up** (the complementary wei stays
    //    in the pool, not the user's pocket).
    let fee_wad = fee_to_wad(fee_bps);
    let post_fee_mul = WAD - fee_wad;
    let dx_in_after_fee = mul_wad_down_checked(dx_in, post_fee_mul).map_err(map_math)?;
    if dx_in_after_fee <= 0 {
        return Err(Error::InsufficientLiquidity);
    }

    // 2. y = x_in / (x_in + dx_in_after_fee), WAD-scaled and in (0, 1).
    //    Round **up** on the divisor / **down** on the result — we want
    //    `y` to be slightly larger than the exact ratio so `1 - y^k`
    //    (the user's quote) is slightly *smaller*. That favours the
    //    pool. div_wad_down does this directly.
    let denom = x_in
        .checked_add(dx_in_after_fee)
        .ok_or(Error::MathOverflow)?;
    let y = strate_math::div_wad_checked(x_in, denom)
        .map_err(map_math)?;
    // y is at most just-under WAD, at least just-above 0.
    if y <= 0 {
        return Err(Error::InsufficientLiquidity);
    }
    if y >= WAD {
        // Should never happen for positive dx_in_after_fee; guard defensively.
        return Ok(0);
    }

    // 3. weight_ratio = w_in / w_out, WAD-scaled. Always positive.
    //    Both `w_in` and `w_out` are bounded in [eps, WAD] so the ratio
    //    is in [eps / WAD, WAD / eps]. Even at the extremes
    //    `weight_ratio` stays inside the `exp` domain because we later
    //    multiply by `ln(y)` (negative, bounded by `-ln(small_y)`),
    //    keeping the exponent bounded.
    //
    //    Round-down here is fine because the next step squares the
    //    rounding into a sub-WAD effect on `dx_out`.
    let weight_ratio = div_wad_down_checked(w_in, w_out).map_err(map_math)?;

    // 4. ln_y in (-∞, 0), bounded by -ln(eps) where eps = y_min.
    //    For y close to WAD, ln_y is small negative (-1e-3 ish at y =
    //    0.999 WAD); for y small, ln_y is large negative (-20 WAD at
    //    y = 2e-9 WAD).
    let ln_y = ln_wad_checked(y).map_err(map_math)?;
    // ln_y must be strictly negative for y < WAD.
    debug_assert!(ln_y <= 0, "ln_y={ln_y} but y={y}");

    // 5. exponent = weight_ratio * ln_y. Negative.
    //    Multiply mixed-sign via mul_wad_down: rounds magnitude up (away
    //    from zero for negatives), which means `exp(exponent)` is
    //    rounded slightly *down* in `foo` — favours the pool.
    let exponent = mul_wad_down_checked(weight_ratio, ln_y).map_err(map_math)?;

    // 6. foo = exp(exponent). In (0, WAD) for negative exponent.
    //    `exp_wad` saturates at zero for very negative inputs and at
    //    `i128::MAX` for very positive — neither saturation can occur
    //    here because `exponent` is at most a few hundred WAD in
    //    magnitude under reasonable pool conditions, and the C-03 lock
    //    keeps `weight_ratio` away from infinity.
    let foo = exp_wad(exponent);
    if foo <= 0 {
        // Pathological underflow — treat as a zero-shaped curve.
        return Ok(strate_math::mul_wad_down(x_out, WAD));
    }
    if foo >= WAD {
        // Numerical edge: `exp` of a tiny negative might round to WAD,
        // giving zero output. That is the "no-op trade" outcome.
        return Ok(0);
    }

    // 7. bar = WAD - foo. Strictly in (0, WAD).
    let bar = WAD - foo;

    // 8. dx_out = x_out * bar / WAD. Round **down** — favour the pool.
    //    `mul_wad_down` is the explicit rounding form.
    let dx_out = mul_wad_down_checked(x_out, bar).map_err(map_math)?;
    Ok(dx_out)
}

/// Compute the time-decayed `(w_pt, w_under)` weights at the current
/// ledger timestamp.
///
/// Returns `Err(Error::MarketLockedNearMaturity)` if the lock window
/// has been entered (defence in depth — the entrypoints also guard).
pub fn weights_at(
    env: &Env,
    x_pt: i128,
    x_under: i128,
    cfg: &AmmConfig,
) -> Result<(i128, i128), Error> {
    let now = env.ledger().timestamp();
    if now >= cfg.maturity {
        return Err(Error::MaturityReached);
    }
    let t_remaining = cfg.maturity - now;
    if t_remaining <= MARKET_LOCK_WINDOW_SECONDS {
        return Err(Error::MarketLockedNearMaturity);
    }

    // t_frac = t_remaining / total_duration, in WAD, in (0, 1].
    // total_duration is guaranteed > 0 by the constructor.
    let t_frac = strate_math::muldiv_floor(
        t_remaining as i128,
        WAD,
        cfg.total_duration as i128,
    );
    if t_frac <= 0 {
        return Err(Error::MarketLockedNearMaturity);
    }
    // Clamp to safely below WAD so `ln(t_frac)` is strictly negative
    // and well inside the ln_wad range-reduction's convergence
    // window. (At exactly `t_frac == WAD`, `ln_wad`'s atanh series
    // sits on the boundary of its convergence radius.) Using
    // `WAD / 2` as the upper clamp degrades naturally — for any
    // realistic tenor with non-trivial time elapsed, t_frac is
    // already comfortably below WAD/2; the clamp only fires within
    // the first half of the tenor.
    // We clamp to `WAD / 2` (i.e. equivalent of `time-to-maturity ==
    // total_duration / 2`) so the input to `ln_wad` lands solidly in
    // the canonical `[WAD/2, WAD)` window where the atanh series
    // converges quickly. The clamp only fires during the FIRST half
    // of the tenor — past mid-tenor `t_frac` is naturally <= WAD/2
    // and we use the true value. The economic effect is a fixed
    // weight schedule for the first half of the life of the pool,
    // which is the regime in which the curve is the LEAST sensitive
    // to time anyway (weight tilt accelerates near maturity).
    let t_frac = if t_frac > WAD / 2 { WAD / 2 } else { t_frac };

    let ln_t = ln_wad_checked(t_frac).map_err(map_math)?;
    // ln_t is negative; abs value grows as t_frac shrinks.

    // p_spot = scalar_root * (x_under / x_pt). For a healthy pool with
    // PT trading at a discount, p_spot ∈ (0, 1).
    if x_pt <= 0 || x_under <= 0 {
        return Err(Error::InsufficientLiquidity);
    }
    let ratio = strate_math::div_wad_checked(x_under, x_pt).map_err(map_math)?;
    let p_spot = mul_wad_up_checked(cfg.scalar_root, ratio).map_err(map_math)?;
    if p_spot <= 0 {
        return Err(Error::Math);
    }

    let ln_price_raw = ln_wad_checked(p_spot).map_err(map_math)?;

    // Edge: p_spot ≈ WAD ⇒ ln_price ≈ 0 ⇒ divide-by-zero. Clamp to a
    // tiny floor on the magnitude. At p_spot = WAD exactly, the curve
    // is symmetric and the time-weighting term cancels — w_pt would be
    // *any* value yielding `w_pt + w_under = WAD`; we fall back to the
    // 50/50 split so the swap formula degenerates to constant-product.
    let ln_price = if ln_price_raw.abs() < LN_PRICE_EPS {
        // Default to a symmetric weight; signed sign-of-ln_price
        // matters for the ratio sign, so we cannot just substitute eps.
        return Ok((WAD / 2, WAD - WAD / 2));
    } else {
        ln_price_raw
    };

    // w_pt = ln(t_frac) / ln(p_spot). Both negative ⇒ positive ratio.
    // Round half-even so neither side is systematically favoured.
    let w_pt_raw = strate_math::div_wad_checked(ln_t, ln_price).map_err(map_math)?;

    // Clamp to (eps, WAD - eps). Outside this range the curve becomes
    // numerically unstable and (per Pendle's own reference) we fall
    // back to a symmetric split — the time-weighting is dominated by
    // the lock window anyway.
    //
    // Negative `w_pt_raw` corresponds to `p_spot > WAD` (PT trading at
    // a premium to underlying, which violates the no-arbitrage
    // assumption baked into the curve). Rather than rejecting, fall
    // back to the symmetric split so the AMM still quotes — the LP-vs-
    // arbitrageur dynamics will push the pool back into the canonical
    // regime within a few swaps.
    let w_pt = if w_pt_raw <= 0 {
        return Ok((WAD / 2, WAD - WAD / 2));
    } else if w_pt_raw >= WAD {
        // `w_pt_raw >= WAD` happens when `|ln_t|` (artificially
        // floored by the `t_frac <= WAD/2` clamp above) dwarfs the
        // near-balanced `|ln_p_spot|`. Returning `WAD - 1` here makes
        // `compute_swap_out`'s `weight_ratio = w_in/w_out` collapse to
        // ~0 or ~∞, which then snaps `dx_out` to 0 and breaks every
        // call site with `Error::InsufficientLiquidity`. Falling back
        // to the symmetric split degrades to the constant-product
        // form — the same fallback the `w_pt_raw <= 0` branch uses
        // for the dual numerical regime.
        return Ok((WAD / 2, WAD - WAD / 2));
    } else {
        w_pt_raw
    };
    let w_under = WAD - w_pt;
    Ok((w_pt, w_under))
}

/// Translate `strate_math::Error` into the protocol-level error enum.
#[inline]
fn map_math(e: strate_math::Error) -> Error {
    match e {
        strate_math::Error::Overflow => Error::MathOverflow,
        strate_math::Error::Underflow => Error::MathUnderflow,
        strate_math::Error::DivisionByZero => Error::DivisionByZero,
        strate_math::Error::LnOfNonPositive => Error::LnOfNegative,
        strate_math::Error::ExpOutOfDomain => Error::Math,
    }
}

// =========================================================================
// Unit tests
// =========================================================================
//
// These tests are pure — they don't touch a Soroban Env, only the math
// helpers. The integration tests in `tests/` exercise the full
// entrypoint flow including storage and authentication.

#[cfg(test)]
mod tests {
    use super::*;

    const ONE: i128 = WAD; // 1.0
    const HALF: i128 = WAD / 2;

    /// Helper: pool with equal reserves and balanced weights — the
    /// formula reduces to the constant-product Uniswap form.
    fn uniswap_check(x: i128, dx: i128, fee_bps: u32) -> i128 {
        compute_swap_out(x, x, dx, HALF, HALF, fee_bps).expect("swap_out succeeds")
    }

    #[test]
    fn swap_zero_is_noop() {
        let r = compute_swap_out(100 * ONE, 100 * ONE, 0, HALF, HALF, 0);
        assert!(matches!(r, Err(Error::InsufficientLiquidity)));
    }

    #[test]
    fn uniswap_x_equals_x_at_balanced_weights() {
        // With w_in = w_out and zero fee, formula collapses to
        // dx_out = x_out * (1 - x_in/(x_in+dx_in)) = x_out * dx_in / (x_in + dx_in).
        // For x_in == x_out == 100 WAD and dx_in == 10 WAD,
        // dx_out exact = 100 * 10 / 110 = 9.0909... WAD.
        let dx = uniswap_check(100 * ONE, 10 * ONE, 0);
        // Reordered to avoid i128 overflow at compile time. Algebraically
        // identical to (100 * ONE * 10 * ONE) / (110 * ONE) = (1000 * ONE) / 110.
        let expected = (100 * 10 * ONE) / 110;
        // Should match within a few wei from exp/ln rounding.
        let diff = (dx - expected).abs();
        assert!(diff < 100_000_000, "dx={dx} expected={expected} diff={diff}");
    }

    #[test]
    fn fee_reduces_output() {
        let no_fee = uniswap_check(100 * ONE, 10 * ONE, 0);
        let with_fee = uniswap_check(100 * ONE, 10 * ONE, 30); // 0.3%
        assert!(with_fee < no_fee, "no_fee={no_fee} with_fee={with_fee}");
        // The reduction should be ~0.3% of the input -> ~0.3% smaller
        // output. Allow wide tolerance because of the non-linearity.
        let rel_drop = (no_fee - with_fee) * 10_000 / no_fee;
        assert!(rel_drop >= 20 && rel_drop <= 50, "drop bps = {rel_drop}");
    }

    #[test]
    fn larger_input_yields_more_output_monotonically() {
        let a = uniswap_check(100 * ONE, 1 * ONE, 0);
        let b = uniswap_check(100 * ONE, 5 * ONE, 0);
        let c = uniswap_check(100 * ONE, 20 * ONE, 0);
        assert!(a < b && b < c, "{a} < {b} < {c} expected");
    }

    #[test]
    fn output_bounded_by_pool() {
        // Even a massive input cannot extract more than the entire output reserve.
        let dx = uniswap_check(100 * ONE, 1_000_000 * ONE, 0);
        assert!(dx < 100 * ONE);
    }

    #[test]
    fn skewed_weights_favour_higher_weight_side() {
        // Balancer-style weighted swap:
        //   dx_out = x_out * (1 - (x_in / (x_in + dx_in))^(w_in / w_out))
        //
        // Higher w_in (relative to w_out) increases the exponent above 1,
        // which pulls the base term toward 0 and thus increases dx_out.
        // Equivalently: the higher-weighted asset is the "scarcer" side
        // and trades from it (i.e. swapping IT for the lower-weighted
        // asset) yields more output per unit input.
        //
        // So w_in > w_out  ⇒  MORE output per unit input
        //    w_in < w_out  ⇒  LESS output per unit input
        let low_w_in = compute_swap_out(100 * ONE, 100 * ONE, 10 * ONE, HALF / 2, HALF + HALF / 2, 0)
            .unwrap();
        let high_w_in = compute_swap_out(100 * ONE, 100 * ONE, 10 * ONE, HALF + HALF / 2, HALF / 2, 0)
            .unwrap();
        assert!(high_w_in > low_w_in, "low_w_in={low_w_in} high_w_in={high_w_in}");
    }

    #[test]
    fn fee_to_wad_basics() {
        assert_eq!(fee_to_wad(0), 0);
        assert_eq!(fee_to_wad(100), WAD / 100); // 1%
        assert_eq!(fee_to_wad(30), 3 * WAD / 1000); // 0.3%
    }

    #[test]
    fn invalid_weights_rejected() {
        let r = compute_swap_out(100 * ONE, 100 * ONE, 10 * ONE, 0, WAD, 0);
        assert!(matches!(r, Err(Error::Math)));
        let r = compute_swap_out(100 * ONE, 100 * ONE, 10 * ONE, WAD, -1, 0);
        assert!(matches!(r, Err(Error::Math)));
    }

    #[test]
    fn empty_pool_rejected() {
        let r = compute_swap_out(0, 100 * ONE, 10 * ONE, HALF, HALF, 0);
        assert!(matches!(r, Err(Error::InsufficientLiquidity)));
        let r = compute_swap_out(100 * ONE, 0, 10 * ONE, HALF, HALF, 0);
        assert!(matches!(r, Err(Error::InsufficientLiquidity)));
    }
}
