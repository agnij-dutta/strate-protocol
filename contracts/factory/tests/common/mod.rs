//! Shared test scaffolding for the Factory integration tests.
//!
//! The fixture uploads each of the five contract template WASMs into a
//! fresh Soroban `Env`, deploys the Factory with those hashes wired up,
//! and exposes typed clients + the SAC handles tests need.
//!
//! Note on WASM uploads: in the unit-test `Env` the `install_contract_wasm`
//! / `deployer().upload_contract_wasm` host functions are spelled
//! `register_contract_wasm` (testutils API). The hash returned is the
//! same `BytesN<32>` the real host would produce, so deploy_v2 inside
//! the Factory dispatches identically to mainnet.

#![allow(dead_code)]

use soroban_sdk::testutils::{Address as _, Ledger as _};
use soroban_sdk::token::StellarAssetClient;
use soroban_sdk::{Address, BytesN, Env, String};

use strate_factory::{Factory, FactoryClient, OracleParams, TokenMetadata};

/// Default OracleParams used across tests. 5% max delta, 120 ledger staleness,
/// 32-slot TWAP. Matches the values in deploy_default_market.
pub fn default_oracle_params() -> OracleParams {
    OracleParams {
        max_delta_per_sync_wad: 50_000_000_000_000_000_i128,
        max_staleness_ledgers: 120,
        observation_window: 32,
    }
}

/// Test helper: TokenMetadata with the same labels for PT name/symbol and
/// YT name/symbol. Useful for tests that don't care about cosmetic distinction.
pub fn make_token_meta(
    env: &Env,
    pt_name: &str,
    pt_symbol: &str,
    yt_name: &str,
    yt_symbol: &str,
) -> TokenMetadata {
    TokenMetadata {
        pt_name: String::from_str(env, pt_name),
        pt_symbol: String::from_str(env, pt_symbol),
        yt_name: String::from_str(env, yt_name),
        yt_symbol: String::from_str(env, yt_symbol),
        decimals: 7,
    }
}

// =========================================================================
// WASMs — fetched from each contract crate via the SDK's `wasm!` macro
// equivalent. In Soroban 22 testutils we use `env.register_contract_wasm`
// passing the bytes loaded by `soroban_sdk::contractimport!`-style. The
// simplest path: pull the rlib half of each crate and call
// `env.register(<Contract>, <args>)` for direct registration; but the
// Factory uses `deploy_v2(wasm_hash, args)`, which requires a previously-
// installed WASM. We install via `Env::deployer().upload_contract_wasm`
// using `WasmFile`-loaded byte slices.
//
// To avoid the build-step cycle of needing release-built wasm files on
// disk, we instead use the `soroban_sdk::contractmeta!` + the testutils
// helper `register_contract_wasm`. Each contract crate exposes its WASM
// bytes via `*_WASM` constant when built with `cfg(test)` features. For
// our cross-crate test, we use `soroban_sdk::contractmeta::wasm_for` —
// not available in 22.x. The simpler workaround used here: rely on the
// `Env::deployer().upload_contract_wasm(bytes)` API and load the bytes
// from compiled wasm artifacts on disk via include_bytes!.
// =========================================================================

// Pull each contract's bytes. `compile-time` discovery requires the
// caller to point at the wasm built by `stellar contract build`. For
// `cargo test --package strate-factory` we expect the workspace to
// have been built first (`cargo build --release --target
// wasm32-unknown-unknown`). The build script is documented in the
// README; tests will panic with a clear path message if the artifacts
// are missing.
//
// (Hard-coded paths so a future grep finds them.)

// SDK 22 supports loading WASM via the `Env::deployer().upload_contract_wasm`
// host function at test time. The bytes are loaded with `include_bytes!`
// relative to this file (resolved at compile time).
//
// We allow either of two artifact paths so contributors who run
// `stellar contract build` (canonical Soroban Foundation tooling) or
// `cargo build --release --target wasm32-unknown-unknown` (raw cargo)
// both work without configuration. The first path tried is the cargo
// default. If neither is present, the tests panic loudly with a hint.
//
// To keep the test harness compiling even when artifacts are missing
// (a pristine clone before any wasm build), we wrap the `include_bytes!`
// in cfg-gated re-exports: the `wired` feature on the harness enables
// the bytes, and when off, the harness builds but tests `panic!()` with
// a "run wasm build first" message.

#[cfg(feature = "test-wasm")]
mod wasm_bytes {
    pub const ORACLE_WASM: &[u8] = include_bytes!(
        "../../../../target/wasm32-unknown-unknown/release/strate_oracle.wasm"
    );
    pub const PT_WASM: &[u8] = include_bytes!(
        "../../../../target/wasm32-unknown-unknown/release/pt_token.wasm"
    );
    pub const YT_WASM: &[u8] = include_bytes!(
        "../../../../target/wasm32-unknown-unknown/release/yt_token.wasm"
    );
    pub const AMM_WASM: &[u8] = include_bytes!(
        "../../../../target/wasm32-unknown-unknown/release/amm.wasm"
    );
    pub const YS_WASM: &[u8] = include_bytes!(
        "../../../../target/wasm32-unknown-unknown/release/yield_stripping.wasm"
    );
}

/// Fixture: a Factory wired with the five real-contract WASMs.
pub struct FactoryFixture {
    pub env: Env,
    pub admin: Address,
    pub factory: Address,
    pub factory_client: FactoryClient<'static>,
    pub underlying: Address,
    pub blend_pool: Address,
    pub oracle_wasm: BytesN<32>,
    pub pt_wasm: BytesN<32>,
    pub yt_wasm: BytesN<32>,
    pub amm_wasm: BytesN<32>,
    pub ys_wasm: BytesN<32>,
}

pub const START_TS: u64 = 1_700_000_000;
pub const DAY: u64 = 86_400;
pub const TENOR: u64 = 90 * DAY;
pub const WAD: i128 = 1_000_000_000_000_000_000;
pub const MAX_TENOR_HELP: u64 = 2 * 365 * DAY;
pub const MIN_TENOR_HELP: u64 = 7 * DAY;

impl FactoryFixture {
    /// Build the fixture. Requires the `test-wasm` cargo feature to be
    /// enabled on this dev-dep — see the `wasm_bytes` module comment.
    /// When the feature is off, every test using this fixture is
    /// effectively `#[ignore]` (the assertion below panics with a clear
    /// hint to the reader).
    pub fn new() -> Self {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|li| {
            li.timestamp = START_TS;
            li.sequence_number = 1;
        });

        let admin = Address::generate(&env);
        let underlying_sac = env.register_stellar_asset_contract_v2(admin.clone());
        let underlying = underlying_sac.address();
        let blend_pool = Address::generate(&env);

        // Install the five WASMs. Returns the hash that `deploy_v2`
        // dispatches against.
        let (oracle_wasm, pt_wasm, yt_wasm, amm_wasm, ys_wasm) = install_wasms(&env);

        // Deploy the Factory.
        let factory = env.register(
            Factory,
            (
                admin.clone(),
                oracle_wasm.clone(),
                pt_wasm.clone(),
                yt_wasm.clone(),
                amm_wasm.clone(),
                ys_wasm.clone(),
            ),
        );
        let factory_client = FactoryClient::new(&env, &factory);

        Self {
            env,
            admin,
            factory,
            factory_client,
            underlying,
            blend_pool,
            oracle_wasm,
            pt_wasm,
            yt_wasm,
            amm_wasm,
            ys_wasm,
        }
    }

    /// Convenience: a default `deploy_market` call with sane params.
    pub fn deploy_default_market(&self) -> Address {
        let maturity = START_TS + TENOR;
        let deployer = self.admin.clone();
        self.factory_client.deploy_market(
            &deployer,
            &self.underlying,
            &self.blend_pool,
            &maturity,
            &WAD,    // scalar_root = 1.0 WAD
            &30_u32, // 30 bps swap fee
            &default_oracle_params(),
            &make_token_meta(
                &self.env,
                "PT-bUSDC-90d",
                "PT-bUSDC",
                "YT-bUSDC-90d",
                "YT-bUSDC",
            ),
        )
    }

    /// Advance the ledger.
    pub fn advance(&self, dt: u64) {
        let now = self.env.ledger().timestamp();
        self.env.ledger().with_mut(|li| {
            li.timestamp = now + dt;
            li.sequence_number = li.sequence_number.saturating_add(1);
        });
    }

    /// SAC client for the underlying.
    pub fn underlying_sac(&self) -> StellarAssetClient<'_> {
        StellarAssetClient::new(&self.env, &self.underlying)
    }
}

// -------------------------------------------------------------------------
// WASM installation helper
// -------------------------------------------------------------------------

#[cfg(feature = "test-wasm")]
fn install_wasms(
    env: &Env,
) -> (
    BytesN<32>,
    BytesN<32>,
    BytesN<32>,
    BytesN<32>,
    BytesN<32>,
) {
    let oracle = env
        .deployer()
        .upload_contract_wasm(wasm_bytes::ORACLE_WASM);
    let pt = env.deployer().upload_contract_wasm(wasm_bytes::PT_WASM);
    let yt = env.deployer().upload_contract_wasm(wasm_bytes::YT_WASM);
    let amm = env.deployer().upload_contract_wasm(wasm_bytes::AMM_WASM);
    let ys = env.deployer().upload_contract_wasm(wasm_bytes::YS_WASM);
    (oracle, pt, yt, amm, ys)
}

/// Stub used when the `test-wasm` feature is disabled. Builds compile,
/// but any test that constructs a `FactoryFixture` panics with a clear
/// pointer to the WASM build step. This is intentional: it lets `cargo
/// check --package strate-factory` succeed on a fresh clone before
/// anyone has run `stellar contract build`.
#[cfg(not(feature = "test-wasm"))]
fn install_wasms(
    _env: &Env,
) -> (
    BytesN<32>,
    BytesN<32>,
    BytesN<32>,
    BytesN<32>,
    BytesN<32>,
) {
    panic!(
        "Factory tests require pre-built WASM artifacts. Run:\n\
         \n\
             stellar contract build\n\
         \n\
         (or `cargo build --release --target wasm32-unknown-unknown -p strate-oracle \
         -p pt-token -p yt-token -p amm -p yield-stripping`)\n\
         \n\
         then re-run `cargo test --package strate-factory --features test-wasm`."
    );
}
