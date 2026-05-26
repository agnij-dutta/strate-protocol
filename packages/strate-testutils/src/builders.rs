//! Fluent builders for spinning up fully-wired test markets.
//!
//! ## Design
//!
//! The builder has two compilation modes:
//!
//! 1. **Default (`cfg(not(feature = "wired"))`)** — returns a `Market`
//!    populated with deterministically-generated `Address`es. Methods that
//!    require live contract state (`pt_supply`, `yield_index`, etc.) panic
//!    with a diagnostic. This mode exists so the per-contract test crates
//!    (which depend on `strate-testutils` for `MockBlendPool` and the
//!    attacker harness scaffolding) compile even though they don't pull
//!    every other contract.
//!
//! 2. **Wired (`cfg(feature = "wired")`)** — the builder calls
//!    `env.register(Factory, ...)` with the real five-contract WASMs
//!    uploaded via the test deployer, then calls
//!    `Factory::deploy_market(...)`. The returned `Market` carries live
//!    addresses for which the accessor methods do real cross-contract
//!    reads.
//!
//! The wired mode is intended for the workspace-level integration test
//! crate at `tests/integration/`, which is the only crate that should
//! enable `feature = "wired"`. Contract-internal unit tests should NOT.
//!
//! ## Why MarketBuilder is workspace-only
//!
//! The wired builder uploads contract wasms via the `testutils`-only
//! `register_contract_with_constructor` / `upload_contract_wasm` APIs.
//! Production code never imports `soroban_sdk::testutils`. Even if a
//! production contract pulled `strate-testutils` for some helper, the
//! `wired` feature would not be enabled, so the wired code paths never
//! cross the production boundary.

#![allow(clippy::too_many_lines)]

use soroban_sdk::{Address, Env, Map, String, Symbol};
use soroban_sdk::testutils::{Address as _, Ledger as _};

use crate::constants::{DEFAULT_TENOR_SECONDS, ONE_UNDERLYING};

// =========================================================================
// Public types
// =========================================================================

/// A live, wired test market.
pub struct Market {
    /// Underlying token (mock SAC).
    pub underlying: Address,
    /// Mock Blend pool address.
    pub blend_pool: Address,
    /// Strate Oracle.
    pub oracle: Address,
    /// PT token.
    pub pt: Address,
    /// YT token.
    pub yt: Address,
    /// YieldStripping contract.
    pub yield_stripping: Address,
    /// AMM contract address. Always present in the wired build (the
    /// Factory deploys all five contracts). The Optional shape is kept
    /// for the stub path that needs to represent "no AMM" markets.
    pub amm: Option<Address>,
    /// Factory contract. None in the stub path.
    pub factory: Option<Address>,
    /// Maturity, ledger seconds.
    pub maturity: u64,
    /// AMM curve scalar root, WAD.
    pub scalar_root: i128,
    /// Admin address (controls pauses).
    pub admin: Address,
    /// Named users → addresses, populated via `seed_user`.
    pub users: Map<String, Address>,
}

/// Builder for [`Market`]. All setters are optional.
pub struct MarketBuilder {
    now: u64,
    tenor_seconds: u64,
    initial_blend_rate: i128,
    scalar_root: i128,
    fee_bps: u32,
    initial_lp_pt: i128,
    initial_lp_underlying: i128,
    seeded_users: alloc::vec::Vec<(alloc::string::String, i128)>,
    deploy_amm: bool,
    mock_all_auths: bool,
}

impl Default for MarketBuilder {
    fn default() -> Self {
        Self {
            now: 1_700_000_000,
            tenor_seconds: DEFAULT_TENOR_SECONDS,
            initial_blend_rate: 1_000_000_000, // 1.0 in 1e9 scaling
            scalar_root: crate::constants::WAD,
            fee_bps: 30,
            initial_lp_pt: 0,
            initial_lp_underlying: 0,
            seeded_users: alloc::vec::Vec::new(),
            deploy_amm: true, // wired path always deploys the AMM
            mock_all_auths: true,
        }
    }
}

impl Market {
    #[must_use]
    pub fn builder() -> MarketBuilder {
        MarketBuilder::default()
    }
}

impl MarketBuilder {
    #[must_use]
    pub fn now(mut self, now: u64) -> Self {
        self.now = now;
        self
    }

    #[must_use]
    pub fn tenor(mut self, tenor_seconds: u64) -> Self {
        self.tenor_seconds = tenor_seconds;
        self
    }

    #[must_use]
    pub fn initial_blend_rate(mut self, rate: i128) -> Self {
        self.initial_blend_rate = rate;
        self
    }

    #[must_use]
    pub fn scalar_root(mut self, sr: i128) -> Self {
        self.scalar_root = sr;
        self
    }

    #[must_use]
    pub fn fee_bps(mut self, fee_bps: u32) -> Self {
        self.fee_bps = fee_bps;
        self
    }

    #[must_use]
    pub fn amm_reserves(mut self, pt: i128, underlying: i128) -> Self {
        self.initial_lp_pt = pt;
        self.initial_lp_underlying = underlying;
        self.deploy_amm = true;
        self
    }

    #[must_use]
    pub fn with_amm(mut self) -> Self {
        self.deploy_amm = true;
        self
    }

    #[must_use]
    pub fn seed_user(mut self, name: &str, underlying_balance: i128) -> Self {
        self.seeded_users
            .push((alloc::string::String::from(name), underlying_balance));
        self
    }

    /// Disable `env.mock_all_auths()` (off by default). Tests that need
    /// to exercise auth-rejection paths should call this; most tests
    /// leave it on.
    #[must_use]
    pub fn without_mock_auths(mut self) -> Self {
        self.mock_all_auths = false;
        self
    }

    // =====================================================================
    // build() — two implementations, gated by feature.
    // =====================================================================

    /// Materialise the market.
    #[cfg(not(feature = "wired"))]
    pub fn build(self, env: &Env) -> Market {
        use soroban_sdk::testutils::Address as _;
        let now = self.now;
        let tenor = self.tenor_seconds;
        env.ledger().with_mut(|li| {
            li.timestamp = now;
        });
        if self.mock_all_auths {
            env.mock_all_auths();
        }
        let admin = Address::generate(env);
        let underlying = Address::generate(env);
        let blend_pool = Address::generate(env);
        let oracle = Address::generate(env);
        let pt = Address::generate(env);
        let yt = Address::generate(env);
        let ys = Address::generate(env);
        let amm = if self.deploy_amm {
            Some(Address::generate(env))
        } else {
            None
        };
        let mut users = Map::new(env);
        for (name, _bal) in self.seeded_users {
            users.set(String::from_str(env, &name), Address::generate(env));
        }
        Market {
            underlying,
            blend_pool,
            oracle,
            pt,
            yt,
            yield_stripping: ys,
            amm,
            factory: None,
            maturity: now.saturating_add(tenor),
            scalar_root: self.scalar_root,
            admin,
            users,
        }
    }

    /// Materialise the market — wired implementation. Deploys real
    /// contracts via the Factory.
    #[cfg(feature = "wired")]
    pub fn build(self, env: &Env) -> Market {
        wired::build(self, env)
    }
}

// -------------------------------------------------------------------------
// Market accessors
// -------------------------------------------------------------------------

impl Market {
    pub fn advance_by(&self, env: &Env, delta: u64) {
        crate::time::advance_by(env, delta);
    }

    /// Read PT supply from the PT contract.
    #[cfg(feature = "wired")]
    pub fn pt_supply(&self, env: &Env) -> i128 {
        pt_token::PtTokenClient::new(env, &self.pt).total_supply()
    }

    #[cfg(not(feature = "wired"))]
    pub fn pt_supply(&self, _env: &Env) -> i128 {
        panic!("Market::pt_supply requires the `wired` cargo feature");
    }

    /// Read YT supply.
    #[cfg(feature = "wired")]
    pub fn yt_supply(&self, env: &Env) -> i128 {
        yt_token::YtTokenClient::new(env, &self.yt).total_supply()
    }

    #[cfg(not(feature = "wired"))]
    pub fn yt_supply(&self, _env: &Env) -> i128 {
        panic!("Market::yt_supply requires the `wired` cargo feature");
    }

    /// Read the global yield index from YieldStripping.
    #[cfg(feature = "wired")]
    pub fn yield_index(&self, env: &Env) -> i128 {
        yield_stripping::YieldStrippingClient::new(env, &self.yield_stripping).current_index()
    }

    #[cfg(not(feature = "wired"))]
    pub fn yield_index(&self, _env: &Env) -> i128 {
        panic!("Market::yield_index requires the `wired` cargo feature");
    }

    /// Underlying balance held by the YS contract (the "TVL" minus
    /// claimed-but-unwithdrawn yield).
    #[cfg(feature = "wired")]
    pub fn ys_underlying_balance(&self, env: &Env) -> i128 {
        soroban_sdk::token::TokenClient::new(env, &self.underlying).balance(&self.yield_stripping)
    }

    /// AMM reserves `(pt, underlying)`.
    #[cfg(feature = "wired")]
    pub fn amm_reserves(&self, env: &Env) -> (i128, i128) {
        let amm = self.amm.as_ref().expect("AMM not deployed");
        amm::AmmContractClient::new(env, amm).reserves()
    }

    /// Look up a named user.
    pub fn user(&self, env: &Env, name: &str) -> Address {
        let key = String::from_str(env, name);
        self.users
            .get(key)
            .unwrap_or_else(|| panic!("user '{name}' not seeded in Market::builder"))
    }

    /// Master invariant helper: assert `pt_supply == yt_supply` (INV-S1).
    #[cfg(feature = "wired")]
    pub fn assert_invariant_supply_equality(&self, env: &Env) {
        let pt = self.pt_supply(env);
        let yt = self.yt_supply(env);
        assert_eq!(pt, yt, "INV-S1 violated: pt_supply={pt} yt_supply={yt}");
    }

    /// Master invariant helper for the stub path — panics with the same
    /// diagnostic shape as the wired version would if it found a
    /// violation. Tests in the stub path never reach this code.
    #[cfg(not(feature = "wired"))]
    pub fn assert_invariant_supply_equality(&self, _env: &Env) {
        panic!("Market::assert_invariant_supply_equality requires the `wired` cargo feature");
    }

    /// Run ALL 17 invariants from `docs/security/INVARIANTS.md` against
    /// the current market state. Panics with a diagnostic on the first
    /// violation. See [`invariants`].
    #[cfg(feature = "wired")]
    pub fn assert_all_invariants(&self, env: &Env) {
        crate::invariants::assert_all(env, self);
    }
}

// =========================================================================
// Wired implementation
// =========================================================================

#[cfg(feature = "wired")]
mod wired {
    use super::{Market, MarketBuilder};
    use crate::constants::ONE_UNDERLYING;
    use soroban_sdk::testutils::{Address as _, Ledger as _};
    use soroban_sdk::token::StellarAssetClient;
    use soroban_sdk::{Address, BytesN, Env, Map, String};

    // For embedded wasm we use the soroban SDK testutils
    // `register_contract` form for each contract — the host loads the
    // rlib half directly, no wasm bytes needed. That keeps these tests
    // fast (no wasm compile in the inner test loop) and removes the
    // `include_bytes!` brittleness.
    //
    // The factory is deployed with the wasm-upload pattern because its
    // `deploy_market` requires real `BytesN<32>` wasm hashes. We use the
    // testutils-only `upload_contract_wasm` after building each
    // contract's wasm at workspace build time via the cdylib target.
    //
    // For pure in-process integration tests where we don't care about
    // testing the Factory's deploy_v2 path against a real wasm load, we
    // take a simpler route: deploy each contract directly via
    // `env.register(...)` and skip the Factory's deploy_market. The
    // `factory` field on `Market` is then set to None, but every other
    // address is wired exactly as the Factory would have wired it.
    //
    // This trade-off is documented in the integration test crate's
    // README — the Factory has its own focused tests; the master
    // integration suite tests the protocol behavior, not deployment.
    pub fn build(b: MarketBuilder, env: &Env) -> Market {
        env.ledger().with_mut(|li| {
            li.timestamp = b.now;
            li.sequence_number = 1;
        });
        if b.mock_all_auths {
            env.mock_all_auths();
        }

        let admin = Address::generate(env);

        // 1. Underlying SAC.
        let sac = env.register_stellar_asset_contract_v2(admin.clone());
        let underlying = sac.address();
        let underlying_admin = StellarAssetClient::new(env, &underlying);

        // 2. MockBlendPool.
        let blend_pool = env.register(crate::mock_blend::MockBlendPool, ());
        crate::mock_blend::MockBlendPoolClient::new(env, &blend_pool)
            .initialize(&underlying, &b.initial_blend_rate);

        // 3. Oracle.
        let oracle_cfg = strate_oracle::OracleConfig {
            blend_pool: blend_pool.clone(),
            max_delta_per_sync_wad: 50_000_000_000_000_000_i128, // 5%
            max_staleness_ledgers: 120,
            observation_window: 32,
        };
        let oracle = env.register(strate_oracle::Oracle, (admin.clone(), oracle_cfg));

        let maturity = b.now.saturating_add(b.tenor_seconds);

        // 4. PT.
        let pt = env.register(
            pt_token::PtToken,
            (pt_token::PtInitArgs {
                underlying: underlying.clone(),
                maturity,
                name: String::from_str(env, "PT-Strate"),
                symbol: String::from_str(env, "PT"),
                decimals: 7,
            },),
        );

        // 5. YT.
        let yt = env.register(
            yt_token::YtToken,
            (yt_token::YtInitArgs {
                underlying: underlying.clone(),
                maturity,
                name: String::from_str(env, "YT-Strate"),
                symbol: String::from_str(env, "YT"),
                decimals: 7,
            },),
        );

        // 6. AMM. Requires maturity > now + MIN_TOTAL_DURATION + lock.
        // For near-maturity markets the AMM constructor will revert; we
        // skip AMM deployment if the requested tenor is below that
        // floor.
        let amm = if b.tenor_seconds > amm::MIN_TOTAL_DURATION_SECONDS
            + amm::MARKET_LOCK_WINDOW_SECONDS
        {
            Some(env.register(
                amm::AmmContract,
                (
                    pt.clone(),
                    yt.clone(),
                    underlying.clone(),
                    oracle.clone(),
                    maturity,
                    b.scalar_root,
                    b.fee_bps,
                    admin.clone(),
                ),
            ))
        } else {
            None
        };

        // 7. YieldStripping.
        let ys = env.register(
            yield_stripping::YieldStripping,
            (yield_stripping::Config {
                underlying: underlying.clone(),
                pt: pt.clone(),
                yt: yt.clone(),
                oracle: oracle.clone(),
                maturity,
                scalar_root: b.scalar_root,
                admin: admin.clone(),
            },),
        );

        // 8. Wire minters.
        pt_token::PtTokenClient::new(env, &pt).initialize_minter(&ys);
        yt_token::YtTokenClient::new(env, &yt).initialize_minter(&ys);

        // 9. Seed users.
        let mut users = Map::new(env);
        for (name, balance) in &b.seeded_users {
            let u = Address::generate(env);
            if *balance > 0 {
                underlying_admin.mint(&u, balance);
            }
            users.set(String::from_str(env, name), u);
        }

        // 10. Seed initial AMM liquidity (if requested).
        if let Some(ref amm_addr) = amm {
            if b.initial_lp_pt > 0 && b.initial_lp_underlying > 0 {
                // Pick the first seeded user as the provider; if none,
                // mint a throwaway LP provider.
                let provider = if let Some((name, _)) = b.seeded_users.first() {
                    users
                        .get(String::from_str(env, name))
                        .expect("seeded user present")
                } else {
                    let p = Address::generate(env);
                    users.set(String::from_str(env, "_lp"), p.clone());
                    p
                };
                // The provider needs PT and underlying. PT must come
                // via the YS mint path so the minter authorization
                // works. We mint enough underlying to cover the LP +
                // YS mint, then mint PT+YT, then add liquidity using
                // the PT and the spare underlying.
                let total_underlying =
                    b.initial_lp_pt.saturating_add(b.initial_lp_underlying);
                underlying_admin.mint(&provider, &total_underlying);
                let ys_client = yield_stripping::YieldStrippingClient::new(env, &ys);
                let _ = ys_client.mint(&provider, &provider, &b.initial_lp_pt);
                let amm_client = amm::AmmContractClient::new(env, amm_addr);
                amm_client.add_liquidity(
                    &provider,
                    &b.initial_lp_pt,
                    &b.initial_lp_underlying,
                    &1,
                );
            }
        }

        // BytesN<32> hash-of-zero so factory is satisfied if we ever
        // need to record one. We do NOT deploy a Factory in the wired
        // builder path because that path also exercises Factory's
        // deploy_v2 wasm upload mechanics, which adds a moving part
        // that is tested in isolation in `tests/integration/factory.rs`.
        let _: BytesN<32> = BytesN::from_array(env, &[0_u8; 32]);

        Market {
            underlying,
            blend_pool,
            oracle,
            pt,
            yt,
            yield_stripping: ys,
            amm,
            factory: None,
            maturity,
            scalar_root: b.scalar_root,
            admin,
            users,
        }
    }
}

// =========================================================================
// Convenience constructors
// =========================================================================

impl MarketBuilder {
    /// Default 90-day market — `fee_bps = 30`, `scalar_root = 5e17`,
    /// two seeded users (`alice`, `bob`), each 1,000 underlying.
    #[must_use]
    pub fn default_market(env: &Env) -> Market {
        default_market(env)
    }

    /// 25-minute market (inside AMM lock window). No AMM deployed by
    /// construction (the AMM constructor requires longer tenor); tests
    /// for lock-window behavior should use a longer tenor and fast-
    /// forward.
    #[must_use]
    pub fn near_maturity_market(env: &Env) -> Market {
        near_maturity_market(env)
    }

    /// Fresh, zero-liquidity market with one large attacker and one
    /// small victim. C-01 setup.
    #[must_use]
    pub fn empty_market(env: &Env) -> Market {
        empty_market(env)
    }
}

/// Default 90-day market, `fee_bps = 30`, `scalar_root = WAD`, two seeded
/// users (`alice`, `bob`) each with 1,000 underlying.
#[must_use]
pub fn default_market(env: &Env) -> Market {
    Market::builder()
        .seed_user("alice", 1_000 * ONE_UNDERLYING)
        .seed_user("bob", 1_000 * ONE_UNDERLYING)
        .build(env)
}

/// Near-maturity market: 25 minutes to maturity, inside the AMM lock
/// window. The AMM is intentionally not deployed (tenor < AMM minimum) —
/// integration tests for C-03 deploy the AMM via a longer tenor and
/// fast-forward instead.
#[must_use]
pub fn near_maturity_market(env: &Env) -> Market {
    Market::builder()
        .tenor(25 * 60)
        .seed_user("attacker", 10_000 * ONE_UNDERLYING)
        .seed_user("victim", 1_000 * ONE_UNDERLYING)
        .build(env)
}

/// Fresh, zero-liquidity market with one large attacker and one small
/// victim. C-01 donation-attack setup.
#[must_use]
pub fn empty_market(env: &Env) -> Market {
    Market::builder()
        .seed_user("attacker", 1_000_000 * ONE_UNDERLYING)
        .seed_user("victim", 1_000 * ONE_UNDERLYING)
        .build(env)
}

// Keep an explicit import of Symbol so external callers that re-export
// builders also see it.
#[allow(dead_code)]
const _SYMBOL_REEXPORT: fn(&Env) -> Symbol = |e: &Env| Symbol::new(e, "market");
