//! `strate-integration-tests` — workspace-level cross-contract integration tests.
//!
//! Each test in `tests/*.rs` deploys a complete Strate market (Oracle +
//! PT + YT + AMM + YieldStripping, optionally Factory) and exercises a
//! flow that touches at least three contracts. The per-contract crates
//! have their own focused tests; this crate is where we prove the call
//! graph from ADR-001 §"Critical user flows" hangs together end-to-end.
//!
//! ## Crate shape
//!
//! - `src/lib.rs` (this file) — shared helpers (`Fixture`, advance, fund,
//!   read-helpers).
//! - `tests/flow_*.rs` — one happy-path or sad-path per flow.
//! - `tests/attack_*.rs` — driven by the harnesses in
//!   `strate_testutils::attackers::*`. These are the regression tests for
//!   the four critical bug classes (C-01, C-02, C-03, H-01).
//! - `tests/invariants_master.rs` — proptest harness running random op
//!   sequences and asserting every invariant from INVARIANTS.md after each
//!   op.
//!
//! ## Why a dedicated crate
//!
//! `strate-testutils` cannot depend on the five contract crates as
//! non-dev dependencies without forming a cycle (each contract depends on
//! testutils as a dev-dep). The dependency is broken by gating the
//! "wired" contract imports behind the `wired` cargo feature on
//! `strate-testutils` and enabling that feature **only** in this crate.
//! Production binaries of any contract therefore never compile any wired
//! code path.

#![allow(clippy::module_name_repetitions)]
#![allow(clippy::missing_panics_doc)]

use soroban_sdk::token::{StellarAssetClient, TokenClient};
use soroban_sdk::{Address, Env};

use strate_testutils::{
    builders::{Market, MarketBuilder},
    constants::ONE_UNDERLYING,
    mock_blend::MockBlendPoolClient,
    time,
};

// =========================================================================
// Test fixture
// =========================================================================

/// Common fixture wrapping a wired [`Market`] plus shortcuts for the most
/// frequent test interactions. Constructed once per test.
pub struct Fixture {
    pub env: Env,
    pub market: Market,
}

impl Fixture {
    /// 90-day market with two seeded users (`alice` & `bob`), each
    /// carrying 1,000 underlying. AMM deployed and seeded with
    /// 1,000 PT + 1,000 underlying.
    pub fn default_market() -> Self {
        let env = Env::default();
        let market = MarketBuilder::default()
            .seed_user("alice", 1_000 * ONE_UNDERLYING)
            .seed_user("bob", 1_000 * ONE_UNDERLYING)
            .seed_user("carol", 1_000 * ONE_UNDERLYING)
            .build(&env);
        Self { env, market }
    }

    /// 25-minute market (inside AMM lock window). AMM is NOT deployed
    /// because its constructor requires `tenor > MIN_TOTAL_DURATION +
    /// lock_window` — tests for C-03 must use a longer tenor and fast-
    /// forward time.
    pub fn near_maturity_market() -> Self {
        let env = Env::default();
        let market = MarketBuilder::default()
            .tenor(25 * 60)
            .seed_user("attacker", 10_000 * ONE_UNDERLYING)
            .seed_user("victim", 1_000 * ONE_UNDERLYING)
            .build(&env);
        Self { env, market }
    }

    /// Same as default_market but with no seeded LP — useful for C-01.
    pub fn empty_market() -> Self {
        let env = Env::default();
        let market = MarketBuilder::default()
            .seed_user("attacker", 1_000_000 * ONE_UNDERLYING)
            .seed_user("victim", 1_000 * ONE_UNDERLYING)
            .build(&env);
        Self { env, market }
    }

    // ---- Shortcuts -------------------------------------------------------

    pub fn user(&self, name: &str) -> Address {
        self.market.user(&self.env, name)
    }

    pub fn fund(&self, who: &Address, amount: i128) {
        StellarAssetClient::new(&self.env, &self.market.underlying).mint(who, &amount);
    }

    pub fn underlying_balance(&self, who: &Address) -> i128 {
        TokenClient::new(&self.env, &self.market.underlying).balance(who)
    }

    pub fn pt_balance(&self, who: &Address) -> i128 {
        pt_token::PtTokenClient::new(&self.env, &self.market.pt).balance(who)
    }

    pub fn yt_balance(&self, who: &Address) -> i128 {
        yt_token::YtTokenClient::new(&self.env, &self.market.yt).balance(who)
    }

    pub fn ys(&self) -> yield_stripping::YieldStrippingClient<'_> {
        yield_stripping::YieldStrippingClient::new(&self.env, &self.market.yield_stripping)
    }

    pub fn oracle(&self) -> strate_oracle::OracleClient<'_> {
        strate_oracle::OracleClient::new(&self.env, &self.market.oracle)
    }

    pub fn amm(&self) -> amm::AmmContractClient<'_> {
        let addr = self.market.amm.as_ref().expect("AMM not deployed");
        amm::AmmContractClient::new(&self.env, addr)
    }

    pub fn mock_blend(&self) -> MockBlendPoolClient<'_> {
        MockBlendPoolClient::new(&self.env, &self.market.blend_pool)
    }

    pub fn advance_days(&self, days: u64) {
        time::advance_by(&self.env, days * 86_400);
        // Keep the Oracle cache fresh so subsequent YS reads don't
        // trip the staleness gate. In production a keeper would do
        // this; in tests we fold it into the time-advance helper so
        // individual tests don't have to remember.
        let _ = self.oracle().try_sync_rate();
    }

    pub fn advance_to_maturity(&self) {
        time::advance_to(&self.env, self.market.maturity);
        // No sync here — post-maturity the YS path doesn't read the
        // oracle (the yield index is frozen). Tests exercising
        // pre-maturity behaviour use `advance_days`.
    }

    /// Drive the Oracle one sync forward. Used between rate bumps.
    pub fn sync(&self) {
        let _ = self.oracle().try_sync_rate();
        self.ys().sync_yield_index();
    }

    /// Run every invariant check from INVARIANTS.md against the current
    /// state. Panics on any violation.
    pub fn assert_invariants(&self) {
        self.market.assert_all_invariants(&self.env);
    }
}

// =========================================================================
// Helpers exposed to test files
// =========================================================================

/// 1.0 in Blend's 1e9 fixed-point.
pub const BLEND_ONE: i128 = 1_000_000_000;

/// Convenience: bump the mock Blend rate by `rel_wad` and call both
/// Oracle and YS sync. Used by flow tests that simulate yield accrual.
///
/// The mock Blend pool does not actually accrue interest on the
/// underlying it holds — it just reports a rate. To keep the YS
/// solvent enough to pay the yield it has just been told it owes,
/// we top the YS up with the exact face value of the new accrual
/// (`yt_supply * index_delta / WAD`), measured against the live
/// global index after the sync pushes the new value. The unit
/// tests use the same trick via `fund(&f.ys, ...)`. In production
/// this growth comes from Blend's `b_rate` appreciation on the
/// deposited principal.
pub fn accrue(f: &Fixture, rel_wad: i128) {
    let pre_index = f.ys().current_index();
    let mock = f.mock_blend();
    let r = mock.b_rate();
    mock.set_rate(&strate_testutils::mock_blend::bumped_rate(r, rel_wad));
    f.sync();
    let post_index = f.ys().current_index();
    let yt_supply = f.market.yt_supply(&f.env);
    let delta_index = post_index.saturating_sub(pre_index);
    let top_up = yt_supply.saturating_mul(delta_index) / strate_testutils::constants::WAD;
    // Round up by 1 wei to absorb the YS↔YT muldiv-down rounding so
    // INV-S2 has strictly enough backing.
    let top_up = top_up + 1;
    if top_up > 0 {
        StellarAssetClient::new(&f.env, &f.market.underlying)
            .mint(&f.market.yield_stripping, &top_up);
    }
}
