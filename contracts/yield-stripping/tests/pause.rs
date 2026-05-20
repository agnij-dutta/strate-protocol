//! Pause / unpause behaviour.
//!
//! ADR-002 §"Emergency pause": pause is instant, unpause requires a
//! 48-hour timelock. While paused: mint is rejected; claim and redeem
//! remain open.

#![cfg(test)]

mod common;

use common::{Fixture, ONE, WAD};

#[test]
fn paused_blocks_mint_but_not_claim_or_redeem() {
    let f = Fixture::new();
    let alice = f.user(1_000 * ONE);

    // Pre-pause: mint succeeds.
    f.ys_client().mint(&alice, &alice, &(50 * ONE));

    // Accrue something so claim has a value.
    f.advance(1 * common::DAY);
    f.set_rate(WAD + WAD / 100);
    f.fund(&f.ys, 10 * ONE);
    f.ys_client().sync_yield_index();

    // Pause.
    f.ys_client().pause();
    assert!(f.ys_client().is_paused());

    // Mint must revert.
    let res = f.ys_client().try_mint(&alice, &alice, &(10 * ONE));
    assert!(res.is_err(), "paused mint should revert");

    // Claim still works.
    let _ = f.ys_client().claim_yield(&alice);

    // Pair redeem still works.
    f.ys_client().redeem_pair(&alice, &ONE);
}

#[test]
fn unpause_requires_timelock() {
    let f = Fixture::new();

    f.ys_client().pause();
    assert!(f.ys_client().is_paused());

    // Propose unpause.
    f.ys_client().propose_unpause();

    // Immediate execute fails (eta not reached).
    let res = f.ys_client().try_execute_unpause();
    assert!(res.is_err(), "early execute_unpause must revert");

    // Advance past the timelock.
    f.advance(48 * 60 * 60 + 1);
    f.ys_client().execute_unpause();
    assert!(!f.ys_client().is_paused());
}

#[test]
fn pause_cancels_open_unpause_proposal() {
    let f = Fixture::new();
    f.ys_client().pause();
    f.ys_client().propose_unpause();
    // Re-pausing wipes the proposal.
    f.ys_client().pause();
    assert_eq!(f.ys_client().unpause_eta(), 0);
}
