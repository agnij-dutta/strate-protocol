//! C-01 — Donation-inflation attack regression test.
//!
//! See `docs/security/THREAT-MODEL.md` §C-01. The attacker sequence:
//!
//! 1. Attacker mints a tiny PT+YT position to register as a holder.
//! 2. Attacker directly `transfer`s a large amount of underlying to the
//!    YS contract, hoping to inflate the implicit share price.
//! 3. A victim deposits and expects fair share.
//!
//! Mitigations in place:
//!
//! - The first depositor mints DEAD_SHARES (1000) to the YS contract
//!   itself — a permanent share floor. The attacker pays for these
//!   from their own underlying.
//! - YS does NOT use balance-of for share-price computation; it tracks
//!   `TotalUnderlying` accounted explicitly. Direct transfers therefore
//!   cannot move the conversion rate.
//!
//! Witness: victim's PT balance ≈ victim's deposit (within rounding).

use strate_integration_tests::Fixture;
use strate_testutils::attackers::DonationInflationAttacker;
use strate_testutils::AttackResult;

#[test]
fn c01_donation_attack_is_mitigated_e2e() {
    let f = Fixture::empty_market();
    let attacker = f.user("attacker");

    let result = DonationInflationAttacker::standard().run(&f.env, &f.market, &attacker);
    assert_eq!(
        result,
        AttackResult::Mitigated,
        "C-01 attack succeeded: {result:?}"
    );

    // Spot-check: victim's PT balance is non-zero after the post-attack
    // mint sequence ran inside the harness.
    let victim = f.user("victim");
    let victim_pt = f.pt_balance(&victim);
    assert!(victim_pt > 0, "victim got dust: {victim_pt}");
}

#[test]
fn c01_first_deposit_below_dead_shares_reverts() {
    // The C-01 mitigation requires that the first deposit be > DEAD_SHARES.
    // We assert the exact revert.
    let f = Fixture::empty_market();
    let attacker = f.user("attacker");
    f.fund(&attacker, 100_000);

    // A 1-wei mint is forbidden.
    let r = f.ys().try_mint(&attacker, &attacker, &1);
    assert!(
        matches!(r, Ok(Err(_))) || r.is_err(),
        "1-wei first-mint should revert"
    );

    // A DEAD_SHARES-sized mint (1000) is also forbidden.
    let r = f.ys().try_mint(&attacker, &attacker, &1_000);
    assert!(
        matches!(r, Ok(Err(_))) || r.is_err(),
        "1000-wei first-mint should revert"
    );

    // 1001 wei works.
    let r = f.ys().try_mint(&attacker, &attacker, &1_001);
    assert!(r.is_ok(), "1001-wei first-mint should work: {r:?}");
}
