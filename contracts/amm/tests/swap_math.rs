//! Unit tests for the pure swap-math kernel.
//!
//! These tests exercise [`amm::compute_swap_out`] directly. They are
//! free of any Soroban environment, so they pin the closed-form curve
//! behaviour independent of storage / auth.
//!
//! ## Test vectors
//!
//! The Pendle V1 reference is Solidity; replicating Forge differential
//! vectors verbatim requires a Forge harness which is out of scope for
//! this dispatch. Instead, we pin **structural properties** that must
//! hold for any correct Pendle V1 port:
//!
//! 1. At balanced weights, the curve reduces to constant-product
//!    Uniswap V2 — `dx_out = x_out * dx_in / (x_in + dx_in)`. We pin
//!    20 cases against this exact reduction.
//! 2. Higher fees monotonically reduce output.
//! 3. Larger input monotonically increases output (within reserve bounds).
//! 4. Skewed weights favour the lower-weight side (the asset with the
//!    smaller weight is "easier to drain").
//! 5. Output is strictly less than the output reserve.
//!
//! If a follower-up PR introduces forge differential vectors, replace
//! the Uniswap-equivalence pins with exact Solidity outputs.

use amm::compute_swap_out;
use strate_interfaces::Error;

const WAD: i128 = 1_000_000_000_000_000_000;
const HALF: i128 = WAD / 2;

/// Exact Uniswap V2 constant-product formula: `dx_out = x_out * dx_in /
/// (x_in_after_fee + dx_in_after_fee)` with the input rebated by
/// `fee_bps / 10_000`. We compute it at i128 precision since none of
/// the test cases overflow.
fn uniswap_expected(x_in: i128, x_out: i128, dx_in: i128, fee_bps: u32) -> i128 {
    let fee_num = 10_000 - i128::from(fee_bps);
    let dx_in_post = dx_in * fee_num / 10_000;
    // dx_out = x_out * dx_in_post / (x_in + dx_in_post)
    x_out * dx_in_post / (x_in + dx_in_post)
}

/// (x_in, x_out, dx_in, fee_bps). Balanced-weight Uniswap-equivalence vectors.
const UNISWAP_CASES: &[(i128, i128, i128, u32)] = &[
    (100 * WAD, 100 * WAD, 10 * WAD, 0),
    (100 * WAD, 100 * WAD, 10 * WAD, 30),
    (100 * WAD, 100 * WAD, 10 * WAD, 100),
    (1_000 * WAD, 1_000 * WAD, 1 * WAD, 0),
    (100 * WAD, 100 * WAD, 50 * WAD, 0),
    (1_000 * WAD, 100 * WAD, 10 * WAD, 0),
    (100 * WAD, 1_000 * WAD, 10 * WAD, 0),
    (50 * WAD, 200 * WAD, 5 * WAD, 30),
    (200 * WAD, 50 * WAD, 5 * WAD, 30),
    (100 * WAD, 100 * WAD, WAD / 100, 0),
    (10 * WAD, 10 * WAD, 100 * WAD, 0),
    (10_000 * WAD, 10_000 * WAD, 100 * WAD, 100),
    (100 * WAD, 100 * WAD, 10 * WAD, 1),
    (123 * WAD, 456 * WAD, 7 * WAD, 0),
    (456 * WAD, 123 * WAD, 7 * WAD, 0),
    (1_000_000 * WAD, 1_000_000 * WAD, WAD, 0),
    (1_000_000 * WAD, 1_000_000 * WAD, WAD, 30),
    (1 * WAD, 1 * WAD, WAD / 10, 0),
    (50 * WAD, 75 * WAD, WAD, 0),
    (75 * WAD, 50 * WAD, WAD, 0),
];

#[test]
fn balanced_weights_reduce_to_uniswap() {
    // Tolerance: even at balanced weights, the implementation routes
    // through `ln(y)` then `exp(...)` instead of computing `1 -
    // x_in/(x_in+dx_in_post)` directly. The exp/ln round-trip has
    // relative error around 1e-17, so for an `x_out` of `1e6 * WAD ≈
    // 1e24`, drift can reach ~1e7 wei. Allow 1e13 wei (1e-5 WAD) to
    // also cover deep-pool cases without false positives.
    const TOL: i128 = 10_000_000_000_000;
    for (i, &(x_in, x_out, dx_in, fee_bps)) in UNISWAP_CASES.iter().enumerate() {
        let got = compute_swap_out(x_in, x_out, dx_in, HALF, HALF, fee_bps).expect("ok");
        let expected = uniswap_expected(x_in, x_out, dx_in, fee_bps);
        let diff = (got - expected).abs();
        assert!(
            diff <= TOL,
            "case {i}: x_in={x_in} x_out={x_out} dx_in={dx_in} fee={fee_bps}: \
             got={got} expected={expected} diff={diff}"
        );
    }
}

#[test]
fn output_strictly_less_than_pool() {
    for &(x_in, x_out, dx_in, fee_bps) in UNISWAP_CASES.iter() {
        let got = compute_swap_out(x_in, x_out, dx_in, HALF, HALF, fee_bps).unwrap();
        assert!(got < x_out, "got={got} x_out={x_out}");
        assert!(got >= 0);
    }
    // Edge: huge input cannot drain.
    let got = compute_swap_out(WAD, WAD, 1_000_000 * WAD, HALF, HALF, 0).unwrap();
    assert!(got < WAD);
}

#[test]
fn fee_monotonically_reduces_output() {
    let mut prev = i128::MAX;
    for fee in [0u32, 1, 10, 30, 50, 100] {
        let out = compute_swap_out(100 * WAD, 100 * WAD, 10 * WAD, HALF, HALF, fee).unwrap();
        assert!(out <= prev, "fee={fee}: out={out} prev={prev}");
        prev = out;
    }
}

#[test]
fn input_monotonic_in_output() {
    let mut prev = 0;
    for dx in [WAD / 100, WAD / 10, WAD, 5 * WAD, 10 * WAD, 50 * WAD] {
        let out = compute_swap_out(100 * WAD, 100 * WAD, dx, HALF, HALF, 0).unwrap();
        assert!(out > prev, "dx={dx}: out={out} prev={prev}");
        prev = out;
    }
}

#[test]
fn skew_in_output_responds_to_weight_ratio() {
    // dx_out = x_out * (1 - y^(w_in/w_out)) where y = x_in/(x_in+dx_in) ∈ (0,1).
    // For y < 1, y^k is *larger* (closer to 1) when k is small, so
    // (1 - y^k) is *smaller* — i.e. low w_in/w_out ratio → less output.
    // Conversely, high w_in/w_out ratio → larger (1 - y^k) → more output.
    let low_ratio =
        compute_swap_out(100 * WAD, 100 * WAD, 10 * WAD, WAD / 4, 3 * WAD / 4, 0).unwrap();
    let balanced = compute_swap_out(100 * WAD, 100 * WAD, 10 * WAD, HALF, HALF, 0).unwrap();
    let high_ratio =
        compute_swap_out(100 * WAD, 100 * WAD, 10 * WAD, 3 * WAD / 4, WAD / 4, 0).unwrap();
    assert!(
        low_ratio < balanced && balanced < high_ratio,
        "low_ratio={low_ratio} balanced={balanced} high_ratio={high_ratio}"
    );
}

#[test]
fn zero_input_rejected() {
    let r = compute_swap_out(100 * WAD, 100 * WAD, 0, HALF, HALF, 0);
    assert!(matches!(r, Err(Error::InsufficientLiquidity)));
    let r = compute_swap_out(100 * WAD, 100 * WAD, -1, HALF, HALF, 0);
    assert!(matches!(r, Err(Error::InsufficientLiquidity)));
}

#[test]
fn zero_reserves_rejected() {
    let r = compute_swap_out(0, 100 * WAD, 10 * WAD, HALF, HALF, 0);
    assert!(matches!(r, Err(Error::InsufficientLiquidity)));
    let r = compute_swap_out(100 * WAD, 0, 10 * WAD, HALF, HALF, 0);
    assert!(matches!(r, Err(Error::InsufficientLiquidity)));
}

#[test]
fn invalid_weights_rejected() {
    let r = compute_swap_out(100 * WAD, 100 * WAD, 10 * WAD, 0, WAD, 0);
    assert!(matches!(r, Err(Error::Math)));
    let r = compute_swap_out(100 * WAD, 100 * WAD, 10 * WAD, WAD, 0, 0);
    assert!(matches!(r, Err(Error::Math)));
    let r = compute_swap_out(100 * WAD, 100 * WAD, 10 * WAD, -1, WAD, 0);
    assert!(matches!(r, Err(Error::Math)));
}
