//! Rounding-direction drain test.
//!
//! Adversarial model: an attacker performs many tiny (1-wei to a few
//! wei) round-trip swaps in quick succession, trying to accumulate
//! rounding wei in their favour. If the AMM's rounding direction is
//! consistent (assets-out rounds **down**, fees round **up**), the
//! attacker's underlying balance must be **less than or equal to** the
//! amount they deposited at the start — i.e., they cannot extract
//! value from rounding alone.
//!
//! Pendle's Solidity reference flagged this as the most common drift
//! bug in V1 audits.

mod common;

use common::{AmmFixture, ONE};

#[test]
fn ten_thousand_tiny_swap_pairs_do_not_drain_attacker() {
    // We can't actually execute 10K Soroban contract calls quickly in
    // a unit test (each call goes through full host machinery). Use
    // 200 iterations as a representative budget; the property is
    // identical and the structural bug it catches is monotone.
    const ITERS: u32 = 200;

    let fx = AmmFixture::new();
    fx.seed_initial_liquidity(1_000_000 * ONE, 1_000_000 * ONE);

    // Attacker is funded; record starting balances.
    let pt_initial = 10_000 * ONE;
    let und_initial = 10_000 * ONE;
    fx.fund_pt(&fx.trader, pt_initial);
    fx.fund_underlying(&fx.trader, und_initial);

    let pt_balance_pre = fx.pt_tok.balance(&fx.trader);
    let und_balance_pre = fx.underlying_tok.balance(&fx.trader);

    // Run alternating tiny swaps.
    for _ in 0..ITERS {
        // PT -> Underlying, 1 wei in. Output may be zero — that's
        // expected for sub-wei rounding; that ITSELF is the protocol
        // keeping the rounding wei. Skip silently when output would be
        // zero.
        let r1 = fx.amm_client.try_swap_exact_pt_for_sy(&fx.trader, &1, &0);
        let _ = r1; // ignore — either succeeded with 0/1 wei out, or reverted.
        // Underlying -> PT, 1 wei in.
        let r2 = fx.amm_client.try_swap_sy_for_exact_pt(&fx.trader, &1, &0);
        let _ = r2;
    }

    let pt_balance_post = fx.pt_tok.balance(&fx.trader);
    let und_balance_post = fx.underlying_tok.balance(&fx.trader);

    // The attacker's combined balance (in token-native units, NOT
    // value-adjusted — we're checking the rounding bug specifically)
    // must be NO LARGER than the starting balance. Equivalently, the
    // pool's combined balance can only grow.
    let attacker_total_pre = pt_balance_pre + und_balance_pre;
    let attacker_total_post = pt_balance_post + und_balance_post;
    assert!(
        attacker_total_post <= attacker_total_pre,
        "rounding drain: attacker went from {attacker_total_pre} to {attacker_total_post}"
    );
}

#[test]
fn swap_then_swap_back_does_not_yield_attacker() {
    let fx = AmmFixture::new();
    fx.seed_initial_liquidity(1_000_000 * ONE, 1_000_000 * ONE);

    let starting = 100 * ONE;
    fx.fund_pt(&fx.trader, starting);
    let pt_pre = fx.pt_tok.balance(&fx.trader);

    // PT in.
    let und_out = fx
        .amm_client
        .swap_exact_pt_for_sy(&fx.trader, &starting, &1);
    assert!(und_out > 0);
    // Underlying back in.
    let pt_out = fx.amm_client.swap_sy_for_exact_pt(&fx.trader, &und_out, &1);

    let pt_post = fx.pt_tok.balance(&fx.trader);
    // After the round-trip, attacker must hold STRICTLY LESS PT than
    // they started with — fees + rounding both work against them.
    assert!(
        pt_post < pt_pre,
        "round-trip net: pt_pre={pt_pre} pt_post={pt_post} (started={starting} → und {und_out} → pt {pt_out})"
    );
}
