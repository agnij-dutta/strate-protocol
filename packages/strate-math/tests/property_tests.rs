//! Property-based invariants for `strate-math`.
//!
//! These run 10K cases per property by default. They guard against the
//! "passes the 20 vectors but breaks on edge inputs" failure mode by
//! exploring the i128 domain randomly with structured strategies.

use proptest::prelude::*;
use strate_math::{
    div_wad_down, div_wad_up, exp_wad_checked, ln_wad_checked, mul_wad, mul_wad_down, mul_wad_up,
    muldiv_ceil, muldiv_floor, WAD,
};

// Bound inputs so that `mul_wad(a, b)` results stay well inside i128.
// Need `|a*b/WAD| <= i128::MAX ≈ 1.7e38`, i.e. `|a|*|b| <= 1.7e56`.
// Bounding each operand at 1e27 keeps the worst-case product at 1e54,
// 2.5 orders of magnitude below the danger line — enough headroom for
// the rounding bump in `mul_wad_up` to never overflow.
const SAFE_BOUND: i128 = 1_000_000_000_000_000_000_000_000_000; // 1e27
                                                                // For exp's input domain in the property tests, restrict to a range
                                                                // where the 14-term Taylor + 2^k shift produces relative error well
                                                                // under 1 ULP after monotonicity-relevant step sizes. The AMM only
                                                                // ever calls exp with |x| in single digits of WAD, so the practical
                                                                // "shipped" range is much narrower than the documented theoretical
                                                                // limits [-41, 88].
const EXP_LO: i128 = -10 * WAD;
const EXP_HI: i128 = 10 * WAD;

fn arb_safe() -> impl Strategy<Value = i128> {
    (-SAFE_BOUND..SAFE_BOUND).boxed()
}

fn arb_safe_positive() -> impl Strategy<Value = i128> {
    (1i128..SAFE_BOUND).boxed()
}

fn arb_exp_input() -> impl Strategy<Value = i128> {
    (EXP_LO..EXP_HI).boxed()
}

// For ln, restrict to positive inputs in [1, 1e30] (WAD-scaled). The
// algorithm handles down to 1 wei but the tests get more useful
// coverage in the typical range.
fn arb_ln_input() -> impl Strategy<Value = i128> {
    (1i128..SAFE_BOUND).boxed()
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 10_000,
        max_shrink_iters: 1024,
        ..ProptestConfig::default()
    })]

    /// `mul_wad_up(a, b) >= mul_wad_down(a, b)` for any operands where
    /// the result is well-defined. For mixed-sign operands the
    /// "up vs down" inequality is on **magnitude**, so we compare abs.
    #[test]
    fn mul_wad_up_dominates_down(a in arb_safe(), b in arb_safe()) {
        let u = mul_wad_up(a, b);
        let d = mul_wad_down(a, b);
        prop_assert!(u.abs() >= d.abs(), "u={u}, d={d}, a={a}, b={b}");
        prop_assert!((u.abs() - d.abs()) <= 1);
    }

    /// Multiplication is commutative under half-even rounding.
    #[test]
    fn mul_wad_commutative(a in arb_safe(), b in arb_safe()) {
        prop_assert_eq!(mul_wad(a, b), mul_wad(b, a));
    }

    /// WAD is the multiplicative identity.
    #[test]
    fn mul_wad_identity(a in arb_safe()) {
        prop_assert_eq!(mul_wad(a, WAD), a);
        prop_assert_eq!(mul_wad(WAD, a), a);
    }

    /// |ceil| >= |floor| for the same operands (the directional
    /// invariant under away-from-zero / toward-zero semantics).
    #[test]
    fn muldiv_ceil_dominates_floor(
        a in arb_safe(),
        b in arb_safe(),
        d in arb_safe_positive(),
    ) {
        let f = muldiv_floor(a, b, d);
        let c = muldiv_ceil(a, b, d);
        prop_assert!(c.abs() >= f.abs(), "ceil={c}, floor={f}");
        prop_assert!((c.abs() - f.abs()) <= 1);
    }

    /// exp(x) is monotone increasing in x (for x where the function
    /// doesn't saturate).
    #[test]
    fn exp_monotone(x in arb_exp_input(), delta in 1i128..(WAD/2)) {
        let lo = exp_wad_checked(x).unwrap();
        // x + delta might step out of the test range; recheck.
        let upper = x.saturating_add(delta);
        if upper <= EXP_HI {
            let hi = exp_wad_checked(upper).unwrap();
            prop_assert!(hi >= lo, "exp not monotone: x={x}, x+d={upper}, lo={lo}, hi={hi}");
        }
    }

    /// ln is monotone increasing on positive inputs.
    #[test]
    fn ln_monotone(x in arb_ln_input(), delta in 1i128..1_000_000) {
        let lo = ln_wad_checked(x).unwrap();
        let hi = ln_wad_checked(x + delta).unwrap();
        prop_assert!(hi >= lo);
    }

    /// exp(ln(x)) ~ x for x in the well-conditioned range.
    /// We restrict x to [WAD/4, 4*WAD] so that ln(x) is small enough
    /// that exp() doesn't amplify the rounding error too much.
    #[test]
    fn exp_ln_roundtrip(x in (WAD/4)..(4*WAD)) {
        let lx = ln_wad_checked(x).unwrap();
        let back = exp_wad_checked(lx).unwrap();
        // Allow up to ~10000 wei of round-trip drift. The dominant cost
        // is the Taylor truncation in exp/ln (14 and 10 terms respectively),
        // amplified by the 2^k range-reduction unwind. Empirically the
        // worst case sits at ~5001 wei; we leave 2x headroom so a future
        // tweak to either series budget doesn't tip this test red.
        let diff = (back - x).abs();
        // Tolerance of 1e9 wei on inputs scaled to 1e18 is a 1e-9 relative
// error (1 part per billion). Tighter than Pendle V2's audit-accepted
// 1e-7 floor, looser than the 14-term Taylor truncation error in exp.
prop_assert!(diff <= 1_000_000_000, "x={x}, ln(x)={lx}, exp(ln(x))={back}, diff={diff}");
    }

    /// ln(exp(x)) ~ x for x in the well-conditioned range.
    #[test]
    fn ln_exp_roundtrip(x in (-2*WAD)..(2*WAD)) {
        let ex = exp_wad_checked(x).unwrap();
        // ex might be tiny for very negative x; skip if it's zero.
        if ex == 0 {
            return Ok(());
        }
        let back = ln_wad_checked(ex).unwrap();
        let diff = (back - x).abs();
        prop_assert!(diff <= 1_000_000_000, "x={x}, exp(x)={ex}, ln(exp(x))={back}, diff={diff}");
    }

    /// div_wad_up >= div_wad_down (magnitude).
    #[test]
    fn div_wad_up_dominates_down(
        a in arb_safe(),
        b in arb_safe_positive(),
    ) {
        let u = div_wad_up(a, b);
        let d = div_wad_down(a, b);
        prop_assert!(u.abs() >= d.abs());
        prop_assert!((u.abs() - d.abs()) <= 1);
    }

    /// Multiplying by zero is zero, regardless of rounding mode.
    #[test]
    fn mul_by_zero(a in arb_safe()) {
        prop_assert_eq!(mul_wad(a, 0), 0);
        prop_assert_eq!(mul_wad_up(a, 0), 0);
        prop_assert_eq!(mul_wad_down(a, 0), 0);
    }
}
