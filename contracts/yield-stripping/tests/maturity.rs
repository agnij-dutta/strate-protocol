//! Maturity state machine.
//!
//! - Pre-maturity: `redeem_pt` reverts (must use `redeem_pair`).
//! - Post-maturity: `mint` reverts.

#![cfg(test)]

mod common;

use common::{Fixture, ONE};

#[test]
fn pre_maturity_redeem_pt_only_reverts() {
    let f = Fixture::new();
    let alice = f.user(1_000 * ONE);
    f.ys_client().mint(&alice, &alice, &(50 * ONE));

    let res = f.ys_client().try_redeem_pt(&alice, &(10 * ONE));
    assert!(
        res.is_err(),
        "pre-maturity redeem_pt should require maturity to be reached"
    );
}

#[test]
fn post_maturity_mint_reverts() {
    let f = Fixture::new();
    let alice = f.user(1_000 * ONE);
    // First deposit (so InitialBlendRate is latched).
    f.ys_client().mint(&alice, &alice, &(10 * ONE));

    let now = f.env.ledger().timestamp();
    f.advance(f.maturity - now);
    // Now exactly at maturity.

    let res = f.ys_client().try_mint(&alice, &alice, &(10 * ONE));
    assert!(res.is_err(), "post-maturity mint should revert");
}

#[test]
fn post_maturity_redeem_pt_succeeds() {
    let f = Fixture::new();
    let alice = f.user(1_000 * ONE);
    f.ys_client().mint(&alice, &alice, &(50 * ONE));

    let now = f.env.ledger().timestamp();
    f.advance(f.maturity - now);

    let bal = f.pt_client().balance(&alice);
    let out = f.ys_client().redeem_pt(&alice, &bal);
    assert_eq!(out, bal);
}
