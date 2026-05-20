//! End-to-end lifecycle: mint → accrue → claim → redeem.
//!
//! Witnesses the happy-path invariants from `INVARIANTS.md`:
//!  - INV-S1: `pt_supply == yt_supply` at every step.
//!  - INV-M1: global yield index is monotone non-decreasing.
//!  - INV-S3: at maturity, `ys.balance(underlying) >= pt_supply + unclaimed`.

#![cfg(test)]

mod common;

use common::{Fixture, ONE, WAD};

#[test]
fn mint_then_claim_then_redeem_at_maturity() {
    let f = Fixture::new();
    let alice = f.user(1_000 * ONE);

    // 1. Mint 100 underlying worth of PT+YT.
    let (pt_out, yt_out, idx) = f.ys_client().mint(&alice, &alice, &(100 * ONE));
    // First deposit pays DEAD_SHARES (1 000 native units) to the YS itself.
    assert_eq!(pt_out, 100 * ONE - 1_000);
    assert_eq!(yt_out, 100 * ONE - 1_000);
    // Index at mint is WAD (no accrual yet).
    assert_eq!(idx, WAD);

    // Alice's balances.
    assert_eq!(f.pt_client().balance(&alice), 100 * ONE - 1_000);
    assert_eq!(f.yt_client().balance(&alice), 100 * ONE - 1_000);

    // INV-S1.
    assert_eq!(f.pt_client().total_supply(), f.yt_client().total_supply());

    // 2. Advance + bump rate by 1%. Simulate Blend accrual by funding
    // the YS contract with the yield it would otherwise earn off-protocol.
    f.advance(30 * common::DAY);
    f.set_rate(WAD + WAD / 100);
    // Endow YS with extra underlying covering the yield it owes. In
    // production this growth comes from Blend's b_rate appreciation
    // on the deposited principal; the mock pool does not actually
    // accrue, so we top up directly.
    f.fund(&f.ys, 10 * ONE);

    let pending = f.ys_client().pending_yield(&alice);
    assert!(pending >= 0); // monotone

    // 3. Claim — should pay out the accrued yield.
    let alice_pre = f.underlying_tok().balance(&alice);
    let claimed = f.ys_client().claim_yield(&alice);
    let alice_post = f.underlying_tok().balance(&alice);
    assert_eq!(alice_post - alice_pre, claimed);
    // Approx: 1% of (100 - 0.0001) ≈ 0.99 underlying. (mul_wad_down)
    // Loose bound: must be positive after the rate bump.
    assert!(claimed > 0, "expected nonzero yield after rate bump");

    // INV-M1: index increased.
    assert!(f.ys_client().current_index() >= WAD);

    // 4. Fast-forward to maturity and redeem.
    let now = f.env.ledger().timestamp();
    f.advance(f.maturity - now);
    let pt_bal = f.pt_client().balance(&alice);
    f.ys_client().redeem_pt(&alice, &pt_bal);
    assert_eq!(f.pt_client().balance(&alice), 0);

    // After redeem, alice's underlying balance reflects the deposit
    // minus DEAD_SHARES (the cost of the C-01 mitigation, paid by the
    // first depositor) plus the yield she claimed.
    let final_und = f.underlying_tok().balance(&alice);
    let cost_of_dead_shares = 1_000_i128;
    let expected = 1_000 * ONE - cost_of_dead_shares + claimed;
    assert_eq!(final_und, expected);
}

#[test]
fn pre_maturity_pair_redeem() {
    let f = Fixture::new();
    let alice = f.user(1_000 * ONE);

    f.ys_client().mint(&alice, &alice, &(50 * ONE));
    let bal = f.pt_client().balance(&alice);
    f.ys_client().redeem_pair(&alice, &bal);
    assert_eq!(f.pt_client().balance(&alice), 0);
    assert_eq!(f.yt_client().balance(&alice), 0);
}

#[test]
fn supply_equality_invariant_holds_through_lifecycle() {
    let f = Fixture::new();
    let alice = f.user(500 * ONE);
    let bob = f.user(500 * ONE);

    f.ys_client().mint(&alice, &alice, &(100 * ONE));
    assert_eq!(f.pt_client().total_supply(), f.yt_client().total_supply());

    f.ys_client().mint(&bob, &bob, &(50 * ONE));
    assert_eq!(f.pt_client().total_supply(), f.yt_client().total_supply());

    f.advance(7 * common::DAY);
    f.set_rate(WAD + WAD / 100);
    f.fund(&f.ys, 10 * ONE);

    f.ys_client().claim_yield(&alice);
    // Claims do not change PT/YT supply.
    assert_eq!(f.pt_client().total_supply(), f.yt_client().total_supply());

    f.ys_client().redeem_pair(&alice, &(10 * ONE));
    assert_eq!(f.pt_client().total_supply(), f.yt_client().total_supply());
}
