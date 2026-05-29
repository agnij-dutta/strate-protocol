//! Shared test scaffolding for the Strate Oracle integration tests.
//!
//! Each test file builds an `OracleFixture` from this module to spin up
//! a fresh Soroban `Env`, deploy a `MockBlendPool`, deploy the oracle
//! bound to it, and obtain typed clients for both. The default config
//! mirrors the production-like defaults a Factory would deploy:
//!
//! - 5% max delta per sync (`5e16` WAD).
//! - 120 ledger staleness (~10 min at 5s/ledger).
//! - 32-slot ring buffer.

#![allow(dead_code)]

use soroban_sdk::{
    testutils::{Address as _, Ledger as _},
    Address, Env,
};

use strate_oracle::{Oracle, OracleClient, OracleConfig};
use strate_testutils::mock_blend::{MockBlendPool, MockBlendPoolClient};

/// 5% in WAD.
pub const FIVE_PERCENT_WAD: i128 = 50_000_000_000_000_000;
/// 1% in WAD.
pub const ONE_PERCENT_WAD: i128 = 10_000_000_000_000_000;
/// `1.0` in Blend's 1e9 scaling.
pub const BLEND_ONE: i128 = 1_000_000_000;
/// `1.0` in WAD.
pub const WAD: i128 = 1_000_000_000_000_000_000;
/// 48h in seconds.
pub const TIMELOCK_SECS: u64 = 48 * 60 * 60;
/// 5 seconds per ledger.
pub const SECS_PER_LEDGER: u64 = 5;

/// Wired-up Oracle + MockBlendPool fixture.
pub struct OracleFixture {
    pub env: Env,
    pub admin: Address,
    pub blend_pool: Address,
    pub oracle: Address,
    pub underlying: Address,
}

impl OracleFixture {
    /// Default 32-slot, 5%-cap, 120-ledger staleness setup. The oracle
    /// is **not** synced; callers should `sync_rate()` themselves so the
    /// first-sync semantics are explicit per test.
    pub fn new() -> Self {
        Self::with_config(FIVE_PERCENT_WAD, 120, 32, BLEND_ONE)
    }

    /// Tight config: 1% cap, 10-ledger staleness, 8 slots. Useful for
    /// rate-limit and staleness tests.
    pub fn tight() -> Self {
        Self::with_config(ONE_PERCENT_WAD, 10, 8, BLEND_ONE)
    }

    pub fn with_config(
        max_delta_per_sync_wad: i128,
        max_staleness_ledgers: u32,
        observation_window: u32,
        initial_blend_rate: i128,
    ) -> Self {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|li| {
            li.timestamp = 1_700_000_000;
            li.sequence_number = 1;
        });

        let admin = Address::generate(&env);
        let underlying = Address::generate(&env);

        let blend_pool = env.register(MockBlendPool, ());
        let blend_client = MockBlendPoolClient::new(&env, &blend_pool);
        blend_client.initialize(&underlying, &initial_blend_rate);

        let cfg = OracleConfig {
            blend_pool: blend_pool.clone(),
            max_delta_per_sync_wad,
            max_staleness_ledgers,
            observation_window,
        };

        let oracle = env.register(Oracle, (admin.clone(), cfg));

        Self {
            env,
            admin,
            blend_pool,
            oracle,
            underlying,
        }
    }

    pub fn oracle_client(&self) -> OracleClient {
        OracleClient::new(&self.env, &self.oracle)
    }

    pub fn blend_client(&self) -> MockBlendPoolClient {
        MockBlendPoolClient::new(&self.env, &self.blend_pool)
    }

    /// Advance the ledger by `dt` seconds, bumping the sequence so TTL
    /// arithmetic stays well-defined.
    pub fn advance_time(&self, dt: u64) {
        let now = self.env.ledger().timestamp();
        self.env.ledger().with_mut(|li| {
            li.timestamp = now + dt;
            li.sequence_number = li.sequence_number.saturating_add(1);
        });
    }
}

/// Collapse the `Result<Result<T, E>, Result<Error, InvokeError>>` shape
/// returned by Soroban's `try_*` clients into a single `Result<T, ()>`.
/// Tests use this to assert "did the contract revert at all", regardless
/// of whether the revert was a contract-level `Error` or an
/// invoke-level failure.
pub fn flatten<T, E1, E2, E3>(r: Result<Result<T, E1>, Result<E2, E3>>) -> Result<T, ()> {
    match r {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(_)) => Err(()),
        Err(_) => Err(()),
    }
}
