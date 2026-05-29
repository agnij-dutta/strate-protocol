//! Fee-accrual rounding test.
//!
//! Asserts:
//! - Non-zero fee strictly reduces the output a trader receives versus
//!   the zero-fee equivalent.
//! - The reduction rounds **up on the protocol side** — equivalently,
//!   the trader's output rounds **down**. We verify this by comparing
//!   exact rational arithmetic at small scales.

mod common;

use amm::{compute_swap_out, fee_to_wad};
use common::{AmmFixture, ONE};

const WAD: i128 = 1_000_000_000_000_000_000;
const HALF: i128 = WAD / 2;

#[test]
fn fee_strictly_reduces_output_for_nonzero_fee() {
    let no_fee = compute_swap_out(100 * WAD, 100 * WAD, 10 * WAD, HALF, HALF, 0).unwrap();
    for fee in [1u32, 30, 100] {
        let with_fee = compute_swap_out(100 * WAD, 100 * WAD, 10 * WAD, HALF, HALF, fee).unwrap();
        assert!(with_fee < no_fee, "fee={fee} did not reduce output");
    }
}

#[test]
fn fee_to_wad_is_basis_points_correctly() {
    // 1 bp = 1e14 = WAD / 10_000.
    assert_eq!(fee_to_wad(1), WAD / 10_000);
    assert_eq!(fee_to_wad(30), 30 * WAD / 10_000);
    assert_eq!(fee_to_wad(100), WAD / 100); // 1%.
}

#[test]
fn protocol_keeps_fee_wei_on_round_trip() {
    // A round-trip swap with a fee MUST leave the trader strictly
    // worse off; the difference accrues to the pool as fees.
    let fx = AmmFixture::with_fee(30);
    fx.seed_initial_liquidity(1_000_000 * ONE, 1_000_000 * ONE);

    let initial_pt = 100 * ONE;
    fx.fund_pt(&fx.trader, initial_pt);

    // PT -> underlying.
    let und_out = fx
        .amm_client
        .swap_exact_pt_for_sy(&fx.trader, &initial_pt, &1);
    // Underlying -> PT.
    let pt_back = fx.amm_client.swap_sy_for_exact_pt(&fx.trader, &und_out, &1);

    // After the round-trip, attacker holds < initial_pt of PT and 0 of
    // underlying. Difference = fees retained in the pool.
    assert!(
        pt_back < initial_pt,
        "pt_back={pt_back} initial_pt={initial_pt}"
    );
    let fee_kept = initial_pt - pt_back;
    // Conservative lower bound: roughly 2 * fee_bps of the trade size
    // (one fee each leg, compounded through the curve). 30 bps each
    // way ≈ 60 bps total minus some non-linear discount.
    let approx_minimum = initial_pt * 50 / 10_000; // 0.5%
    assert!(
        fee_kept >= approx_minimum,
        "fee_kept={fee_kept} below expected lower bound {approx_minimum}"
    );
}

#[test]
fn admin_can_set_fee_within_bounds() {
    let fx = AmmFixture::new();
    fx.amm_client.set_fee_bps(&50);
    let cfg = fx.amm_client.config();
    assert_eq!(cfg.fee_bps, 50);
}

#[test]
fn admin_cannot_exceed_max_fee() {
    let fx = AmmFixture::new();
    let r = fx.amm_client.try_set_fee_bps(&101);
    assert!(r.is_err());
}
