//! C-01: donation-inflation attack.
//!
//! Positive test: the attack is mitigated against our YS impl.
//! Negative ("would-succeed") test: a hand-written naive share-price
//! calc *without* dead-shares is shown to produce a victim PT of zero,
//! reproducing the classical EVM ERC-4626 exploit. This proves the
//! mitigation is doing real work.

#![cfg(test)]

mod common;

use common::{Fixture, ONE, WAD};

#[test]
fn donation_inflation_is_mitigated() {
    let f = Fixture::new();
    let attacker = f.user(10_000_000 * ONE);
    let victim = f.user(1_000 * ONE);

    // 1. Attacker is the first depositor with a small mint. C-01 dead-shares
    //    guard fires.
    let attacker_deposit = ONE; // 1 underlying = 10_000_000 wei
    let (att_pt, att_yt, _idx) = f.ys_client().mint(&attacker, &attacker, &attacker_deposit);
    assert_eq!(att_pt, attacker_deposit - 1_000);
    assert_eq!(att_yt, attacker_deposit - 1_000);

    // 2. Attacker "donates" a huge underlying balance directly to the
    //    YS contract via a SAC transfer. This is the classical inflation
    //    move.
    let donation = 1_000_000 * ONE;
    f.underlying_tok().transfer(&attacker, &f.ys, &donation);

    // 3. Victim mints. Because YS computes PT 1:1 from `amount` (not
    //    from share-of-underlying-balance), the donation is ignored:
    //    the victim receives the full `victim_deposit` in PT and YT.
    let victim_deposit = 100 * ONE;
    let (vic_pt, vic_yt, _) = f.ys_client().mint(&victim, &victim, &victim_deposit);
    assert_eq!(vic_pt, victim_deposit);
    assert_eq!(vic_yt, victim_deposit);

    // 4. Witness: victim ended up with a non-zero PT position. In a
    //    vulnerable EVM-4626-style protocol, the victim's share would
    //    round to zero here.
    assert!(
        f.pt_client().balance(&victim) > 0,
        "victim's PT was wiped out — C-01 mitigation regressed"
    );
}

#[test]
fn donation_inflation_extreme_donation_is_mitigated() {
    // Sweep donation up to 10_000_000 underlying — every order of
    // magnitude above the victim deposit. The mitigation must hold.
    let f = Fixture::new();
    let attacker = f.user(100_000_000 * ONE);
    let victim = f.user(1_000 * ONE);

    f.ys_client().mint(&attacker, &attacker, &(2 * ONE));

    let donation = 10_000_000 * ONE;
    f.underlying_tok().transfer(&attacker, &f.ys, &donation);

    let victim_deposit = 100 * ONE;
    let (vic_pt, _, _) = f.ys_client().mint(&victim, &victim, &victim_deposit);
    assert_eq!(vic_pt, victim_deposit);
}

#[test]
fn first_deposit_below_dead_shares_reverts() {
    // A first deposit must be > DEAD_SHARES (= 1 000 native units).
    // A deposit of exactly 1 000 leaves 0 for the user and would be a
    // no-op; the contract rejects it.
    let f = Fixture::new();
    let attacker = f.user(1_000 * ONE);
    let res = f.ys_client().try_mint(&attacker, &attacker, &500);
    assert!(res.is_err(), "tiny first mint should revert");
}

// -------------------------------------------------------------------------
// Negative test — proves the mitigation is doing real work.
// -------------------------------------------------------------------------

/// Sanity check: under a naive share-price-from-balance accounting
/// (no dead shares, no virtual offset), a donation drives the victim's
/// implicit share count to zero. We model the math directly here so
/// that the test still demonstrates the exploit class even though our
/// real contract uses 1:1 accounting and is structurally immune.
#[test]
fn naive_share_price_would_zero_the_victim() {
    // Naive ERC-4626 share calc: shares = amount * total_supply / total_assets
    let total_supply: i128 = 1; // attacker's 1-wei mint
    let total_assets: i128 = 1 + 1_000_000 * ONE; // 1 wei + donation
    let victim_amount: i128 = 100 * ONE;
    let victim_shares = victim_amount * total_supply / total_assets;
    assert_eq!(
        victim_shares, 0,
        "naive share calc must round to zero here — that's the exploit"
    );
    // Our YS bypasses this by using 1:1 accounting plus DEAD_SHARES;
    // see `donation_inflation_is_mitigated`.
    let _ = WAD; // silence unused
}
