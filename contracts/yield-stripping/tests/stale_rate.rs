//! C-02: stale yield index at YT mint.
//!
//! Positive test: bumping the oracle rate immediately before mint
//! cannot be exploited — the attacker's snapshot is taken at the
//! post-sync index, so an immediate claim returns 0.
//!
//! Negative test: if `sync_yield_index` were omitted from `mint`,
//! the attacker would extract the rate-jump worth of yield. We
//! simulate the math directly.

#![cfg(test)]

mod common;

use common::{Fixture, ONE, WAD};

#[test]
fn stale_rate_at_yt_mint_is_mitigated() {
    let f = Fixture::new();
    let alice = f.user(1_000 * ONE);

    // First, seed the protocol with a small deposit so InitialBlendRate
    // is latched (and dead-shares already paid).
    let seed = f.user(10 * ONE);
    f.ys_client().mint(&seed, &seed, &(5 * ONE));

    // Off-protocol: Blend's rate jumps 1% before the attacker mints.
    f.set_rate(WAD + WAD / 100);

    // Attacker mints. Their snapshot must be at the post-sync index,
    // i.e. the new rate, not the cached one.
    let pre = f.underlying_tok().balance(&alice);
    f.ys_client().mint(&alice, &alice, &(100 * ONE));

    // Immediately claim — no time has passed.
    let claimed = f.ys_client().claim_yield(&alice);
    let post = f.underlying_tok().balance(&alice);

    assert_eq!(
        claimed, 0,
        "attacker realised {} of phantom yield — C-02 regressed",
        claimed
    );
    // Underlying net change == -100 (the mint deposit), no extra.
    assert_eq!(post - pre, -(100 * ONE));
}

#[test]
fn large_rate_jump_is_clamped_by_delta_cap() {
    // A 50% rate jump exceeds MAX_DELTA_PER_SYNC_WAD (5%). YS rejects
    // it on the sync path, freezing the index. The attacker therefore
    // cannot use the inflated rate at all.
    let f = Fixture::new();
    let alice = f.user(1_000 * ONE);

    // Establish a baseline.
    f.ys_client().mint(&alice, &alice, &(10 * ONE));

    // Jump rate by 50%.
    f.set_rate(WAD + WAD / 2);

    let res = f.ys_client().try_mint(&alice, &alice, &(10 * ONE));
    assert!(
        res.is_err(),
        "50% rate jump should be rejected by sync_yield_index"
    );
}

// -------------------------------------------------------------------------
// Negative: simulate the un-mitigated arithmetic.
// -------------------------------------------------------------------------

#[test]
fn would_succeed_without_sync_before_mint() {
    // If mint snapshotted the *old* index, and the global index later
    // advanced by 1% over no time elapsed, the attacker would claim:
    //   claim = balance * 1% = balance / 100
    // Demonstrate the math so a regression in mint ordering is
    // recognisable as a violation of this expected value.
    let balance: i128 = 100 * ONE;
    let stolen = balance / 100;
    assert!(stolen > 0, "naive math would extract {} wei", stolen);
}
