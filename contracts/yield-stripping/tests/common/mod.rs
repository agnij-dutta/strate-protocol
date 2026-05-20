//! Shared test fixtures for the yield-stripping integration tests.
//!
//! Each `tests/*.rs` file is its own crate, so we share scaffolding
//! through a `common` module imported via `mod common;`.
//!
//! The fixtures deploy a `MockOracle`, the underlying SAC, PT, YT and
//! YieldStripping contracts and wire them as the factory would in
//! production. The `Market` returned has typed clients for each peer.

#![allow(dead_code)]

use soroban_sdk::testutils::{Address as _, Ledger as _};
use soroban_sdk::token::{StellarAssetClient, TokenClient};
use soroban_sdk::{contract, contractimpl, contracttype, Address, Env, String};

use pt_token::{PtInitArgs, PtToken, PtTokenClient};
use yield_stripping::{Config, YieldStripping, YieldStrippingClient};
use yt_token::{YtInitArgs, YtToken, YtTokenClient};

// -------------------------------------------------------------------------
// MockOracle — a controllable rate source for tests.
// -------------------------------------------------------------------------

#[contracttype]
#[derive(Clone)]
pub enum MockOracleKey {
    Rate,
}

#[contract]
pub struct MockOracle;

#[contractimpl]
impl MockOracle {
    pub fn __constructor(env: Env, initial_rate: i128) {
        env.storage()
            .instance()
            .set(&MockOracleKey::Rate, &initial_rate);
    }

    /// Test-only: set the rate without freshness checks.
    pub fn set_rate(env: Env, new_rate: i128) {
        env.storage()
            .instance()
            .set(&MockOracleKey::Rate, &new_rate);
    }

    /// Implements `RateOracle::get_rate`.
    pub fn get_rate(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&MockOracleKey::Rate)
            .unwrap_or(0)
    }
}

// -------------------------------------------------------------------------
// Market fixture
// -------------------------------------------------------------------------

pub const ONE: i128 = 10_000_000; // 7 decimals
pub const WAD: i128 = 1_000_000_000_000_000_000;
pub const DAY: u64 = 86_400;
pub const TENOR: u64 = 90 * DAY;

/// Test fixture for a fully wired Strate market.
///
/// Clients are recreated on every access (as `fn` methods) because
/// the soroban-sdk `*Client<'a>` types are tied to the `Env`
/// lifetime and a self-referential struct is awkward. Each accessor
/// is a single line of code.
pub struct Fixture {
    pub env: Env,
    pub admin: Address,
    pub underlying: Address,
    pub oracle: Address,
    pub pt: Address,
    pub yt: Address,
    pub ys: Address,
    pub maturity: u64,
}

impl Fixture {
    /// Deploy a fresh market with `now = 1_700_000_000` and tenor =
    /// 90 days. Initial Blend rate = WAD.
    pub fn new() -> Self {
        let env = Env::default();
        // Authorize-all so cross-contract auth doesn't trip the
        // require_auth check for every helper. Tests that need to
        // exercise auth use `mock_auths` explicitly.
        env.mock_all_auths();
        env.ledger().with_mut(|li| {
            li.timestamp = 1_700_000_000;
        });

        let admin = Address::generate(&env);

        // 1. Underlying SAC
        let sac = env.register_stellar_asset_contract_v2(admin.clone());
        let underlying = sac.address();

        // 2. Oracle (mock)
        let oracle = env.register(MockOracle, (WAD,));

        // 3. PT
        let pt = env.register(
            PtToken,
            (PtInitArgs {
                underlying: underlying.clone(),
                maturity: 1_700_000_000 + TENOR,
                name: String::from_str(&env, "PT-bUSDC-90d"),
                symbol: String::from_str(&env, "PT-bUSDC"),
                decimals: 7,
            },),
        );

        // 4. YT
        let yt = env.register(
            YtToken,
            (YtInitArgs {
                underlying: underlying.clone(),
                maturity: 1_700_000_000 + TENOR,
                name: String::from_str(&env, "YT-bUSDC-90d"),
                symbol: String::from_str(&env, "YT-bUSDC"),
                decimals: 7,
            },),
        );

        let maturity = 1_700_000_000 + TENOR;

        // 5. YieldStripping
        let ys = env.register(
            YieldStripping,
            (Config {
                underlying: underlying.clone(),
                pt: pt.clone(),
                yt: yt.clone(),
                oracle: oracle.clone(),
                maturity,
                scalar_root: WAD,
                admin: admin.clone(),
            },),
        );

        // 6. Wire minters on PT and YT to YS.
        PtTokenClient::new(&env, &pt).initialize_minter(&ys);
        YtTokenClient::new(&env, &yt).initialize_minter(&ys);

        Self {
            env,
            admin,
            underlying,
            oracle,
            pt,
            yt,
            ys,
            maturity,
        }
    }

    // ---- Client accessors -----------------------------------------------

    pub fn ys_client(&self) -> YieldStrippingClient<'_> {
        YieldStrippingClient::new(&self.env, &self.ys)
    }

    pub fn pt_client(&self) -> PtTokenClient<'_> {
        PtTokenClient::new(&self.env, &self.pt)
    }

    pub fn yt_client(&self) -> YtTokenClient<'_> {
        YtTokenClient::new(&self.env, &self.yt)
    }

    pub fn oracle_client(&self) -> MockOracleClient<'_> {
        MockOracleClient::new(&self.env, &self.oracle)
    }

    pub fn underlying_asset(&self) -> StellarAssetClient<'_> {
        StellarAssetClient::new(&self.env, &self.underlying)
    }

    pub fn underlying_tok(&self) -> TokenClient<'_> {
        TokenClient::new(&self.env, &self.underlying)
    }

    /// Mint `amount` underlying to `who`. Uses the SAC admin mint.
    pub fn fund(&self, who: &Address, amount: i128) {
        self.underlying_asset().mint(who, &amount);
    }

    /// Make a named user with some seeded underlying. Skips the mint
    /// call when `balance == 0` to avoid the SAC's "non-positive amount"
    /// rejection.
    pub fn user(&self, balance: i128) -> Address {
        let u = Address::generate(&self.env);
        if balance > 0 {
            self.fund(&u, balance);
        }
        u
    }

    /// Advance the ledger by `delta` seconds.
    pub fn advance(&self, delta: u64) {
        let now = self.env.ledger().timestamp();
        self.env.ledger().with_mut(|li| {
            li.timestamp = now + delta;
            li.sequence_number = li.sequence_number.saturating_add(1);
        });
    }

    /// Set the oracle's rate to a new WAD-scaled value.
    pub fn set_rate(&self, new_rate: i128) {
        self.oracle_client().set_rate(&new_rate);
    }

    /// Read the current global yield index.
    pub fn index(&self) -> i128 {
        self.ys_client().current_index()
    }
}

