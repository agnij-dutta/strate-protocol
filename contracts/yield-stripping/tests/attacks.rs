//! Named-attack integration tests.
//!
//! One test per known attack class. Each test instantiates the matching
//! harness from `strate_testutils::attackers` and asserts the result is
//! `AttackResult::Mitigated`. A `Succeeded` outcome causes a `panic!` with
//! the extracted-underlying witness, which the CI report surfaces as the
//! failure cause.
//!
//! ## Why named tests AND proptests?
//!
//! Proptests in `invariants.rs` randomise inputs and look for unknown
//! violations. The tests here pin **known** attacks at fixed configs so
//! that a regression of a specific mitigation surfaces as a named failure
//! ("`attack_donation_inflation_is_mitigated` FAILED") rather than as one
//! shrunken counter-example among many.

#![cfg(test)]

use soroban_sdk::Env;

use strate_testutils::attackers::{
    DonationInflationAttacker, NearMaturityDrainAttacker, StaleRateYtMintAttacker,
    YtTransferStaleSnapshotAttacker,
};
use strate_testutils::builders::{empty_market, near_maturity_market, Market};
use strate_testutils::time::{self, DAY};
use strate_testutils::AttackResult;

// -------------------------------------------------------------------------
// C-01 — donation inflation
// -------------------------------------------------------------------------

#[test]
fn attack_donation_inflation_is_mitigated() {
    let env = Env::default();
    let market = empty_market(&env);
    let attacker = market.user(&env, "attacker");

    let harness = DonationInflationAttacker::standard();
    let result = harness.run(&env, &market, &attacker);

    assert!(
        matches!(result, AttackResult::Mitigated),
        "C-01 donation inflation succeeded: {result:?}"
    );
}

#[test]
fn attack_donation_inflation_extreme_donation() {
    // Sweep the donation upward to make sure the mitigation holds even at
    // the largest realistic skew.
    let env = Env::default();
    let market = empty_market(&env);
    let attacker = market.user(&env, "attacker");

    let mut harness = DonationInflationAttacker::standard();
    harness.donation = 10_000_000 * strate_testutils::constants::ONE_UNDERLYING;
    let result = harness.run(&env, &market, &attacker);

    assert!(
        matches!(result, AttackResult::Mitigated),
        "C-01 mitigation degrades at extreme donation: {result:?}"
    );
}

// -------------------------------------------------------------------------
// C-02 — stale yield index at YT mint
// -------------------------------------------------------------------------

#[test]
fn attack_stale_rate_yt_mint_is_mitigated() {
    let env = Env::default();
    let market = Market::builder()
        .seed_user(
            "attacker",
            10_000 * strate_testutils::constants::ONE_UNDERLYING,
        )
        .build(&env);
    let attacker = market.user(&env, "attacker");

    let harness = StaleRateYtMintAttacker::standard();
    let result = harness.run(&env, &market, &attacker);

    assert!(
        matches!(result, AttackResult::Mitigated),
        "C-02 stale-rate YT mint succeeded: {result:?}"
    );
}

#[test]
fn attack_stale_rate_yt_mint_large_jump() {
    // A 50% jump should trip the Oracle freeze, not the sync; either way
    // the attacker must not realise yield.
    let env = Env::default();
    let market = Market::builder()
        .seed_user(
            "attacker",
            10_000 * strate_testutils::constants::ONE_UNDERLYING,
        )
        .build(&env);
    let attacker = market.user(&env, "attacker");

    let mut harness = StaleRateYtMintAttacker::standard();
    harness.rate_jump_rel_wad = 50 * (strate_testutils::constants::WAD / 100);
    let result = harness.run(&env, &market, &attacker);

    assert!(
        matches!(result, AttackResult::Mitigated),
        "C-02 large rate jump succeeded: {result:?}"
    );
}

// -------------------------------------------------------------------------
// C-03 — near-maturity AMM drain
// -------------------------------------------------------------------------

#[test]
fn attack_near_maturity_amm_is_blocked() {
    let env = Env::default();
    let market = near_maturity_market(&env);
    let attacker = market.user(&env, "attacker");

    // 1 second before maturity (deepest in the lock window).
    let harness = NearMaturityDrainAttacker::standard();
    let result = harness.run(&env, &market, &attacker);

    assert!(
        matches!(result, AttackResult::Mitigated),
        "C-03 swap succeeded inside the lock window: {result:?}"
    );
}

#[test]
fn attack_near_maturity_just_outside_window_works_normally() {
    // 31 minutes before maturity — outside the lock window. Swap should
    // succeed and be bounded by `MAX_APY_MOVE_PER_SWAP`.
    let env = Env::default();
    let market = Market::builder()
        .tenor(time::HOUR * 2)
        .amm_reserves(
            1_000 * strate_testutils::constants::ONE_UNDERLYING,
            1_000 * strate_testutils::constants::ONE_UNDERLYING,
        )
        .seed_user(
            "attacker",
            10_000 * strate_testutils::constants::ONE_UNDERLYING,
        )
        .build(&env);
    let attacker = market.user(&env, "attacker");

    let mut harness = NearMaturityDrainAttacker::standard();
    harness.seconds_before_maturity = 31 * 60;
    harness.pt_in = strate_testutils::constants::ONE_UNDERLYING; // 1 PT only
    let result = harness.run(&env, &market, &attacker);

    // Either Mitigated (swap respected the APY cap) is the correct answer.
    // `Succeeded` here means the curve drifted past 5% — also a regression.
    assert!(
        matches!(result, AttackResult::Mitigated),
        "C-03 negative test: swap outside lock window drained pool: {result:?}"
    );
}

// -------------------------------------------------------------------------
// H-01 — YT transfer recipient stale snapshot
// -------------------------------------------------------------------------

#[test]
fn attack_yt_transfer_stale_snapshot_is_mitigated() {
    let env = Env::default();
    let market = Market::builder()
        .seed_user("alice", 1_000 * strate_testutils::constants::ONE_UNDERLYING)
        .seed_user("bob", 0)
        .build(&env);
    let bob = market.user(&env, "bob");

    let harness = YtTransferStaleSnapshotAttacker::standard();
    let result = harness.run(&env, &market, &bob);

    assert!(
        matches!(result, AttackResult::Mitigated),
        "H-01 stale-snapshot YT transfer succeeded: {result:?}"
    );
}

#[test]
fn attack_yt_transfer_full_balance_then_claim() {
    // Edge case: alice transfers 100% to bob then immediately claims.
    // Alice's claim must equal pre-transfer accrued; bob's claim post must
    // equal post-transfer accrued only.
    let env = Env::default();
    let market = Market::builder()
        .seed_user("alice", 1_000 * strate_testutils::constants::ONE_UNDERLYING)
        .seed_user("bob", 0)
        .build(&env);
    let bob = market.user(&env, "bob");

    let mut harness = YtTransferStaleSnapshotAttacker::standard();
    harness.transfer_bps = 10_000; // 100%
    harness.pre_transfer_seconds = 14 * DAY;
    harness.post_transfer_seconds = 1 * DAY;
    let result = harness.run(&env, &market, &bob);

    assert!(
        matches!(result, AttackResult::Mitigated),
        "H-01 full-balance edge: {result:?}"
    );
}

// -------------------------------------------------------------------------
// A-05 — pause griefing near maturity (claim must work post-maturity)
// -------------------------------------------------------------------------

#[test]
#[ignore = "requires pause control + post-maturity claim path on YS contract"]
fn attack_pause_griefing_near_maturity() {
    let env = Env::default();
    let market = Market::builder()
        .tenor(DAY)
        .seed_user("victim", 100 * strate_testutils::constants::ONE_UNDERLYING)
        .build(&env);
    let _ = market;
    // TODO(blockchain-developer):
    //   1. victim mints
    //   2. owner.pause()
    //   3. advance to past maturity
    //   4. victim.claim() and victim.redeem_pt() MUST succeed regardless of pause
}

// -------------------------------------------------------------------------
// A-10 — YT transferred to YS contract itself
// -------------------------------------------------------------------------

#[test]
#[ignore = "requires YT _update hook to reject transfers to YS address"]
fn attack_yt_to_ys_self() {
    let env = Env::default();
    let market = Market::builder()
        .seed_user("alice", 1_000 * strate_testutils::constants::ONE_UNDERLYING)
        .build(&env);
    let _ = market;
    // TODO(blockchain-developer):
    //   1. alice mints
    //   2. alice.yt.transfer(ys_address, balance)
    //   3. assert revert TRANSFER_TO_PROTOCOL_FORBIDDEN
}

// -------------------------------------------------------------------------
// Sanity: the four critical mitigations are also asserted via proptest in
// `invariants.rs`. If a named test passes but the proptest fails, the
// proptest's shrunken counter-example points at an edge case not covered
// by the named config.
// -------------------------------------------------------------------------
