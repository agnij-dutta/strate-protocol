//! Shared scaffolding for the AMM integration tests.
//!
//! Spins up a fresh `Env`, two Stellar Asset Contracts (one for PT, one
//! for underlying), and the AMM bound to them. PT and underlying are
//! intentionally modelled as plain SACs here — the AMM does not require
//! the privileged-mint surface of the real `pt-token` contract. The
//! distinction matters only inside YieldStripping's tests.
//!
//! The oracle is a deterministic mock — the AMM only writes TWAP
//! samples to its own storage; it does not call `oracle.observe` in
//! this v1 (the spec's required cross-contract call list places Oracle
//! integration on the post-v1 roadmap). We expose an `Address` for the
//! oracle so the constructor signature matches the production tuple,
//! but no client is needed.

#![allow(dead_code)]

use soroban_sdk::testutils::{Address as _, Ledger as _};
use soroban_sdk::token::{StellarAssetClient, TokenClient};
use soroban_sdk::{Address, Env};

use amm::{AmmContract, AmmContractClient};

pub const ONE: i128 = 10_000_000; // 7-decimal underlying.
pub const WAD: i128 = 1_000_000_000_000_000_000;
pub const DAY: u64 = 86_400;
pub const TENOR: u64 = 90 * DAY;
pub const START_TS: u64 = 1_700_000_000;

/// Default scalar root: 1.0 WAD. Pendle V1 uses values in [1e18, 1e19];
/// for parity tests the simplest choice that exercises the curve is 1.
pub const DEFAULT_SCALAR_ROOT: i128 = WAD;

/// Default fee: 30 bps (0.3%).
pub const DEFAULT_FEE_BPS: u32 = 30;

/// Wired AMM fixture.
pub struct AmmFixture {
    pub env: Env,
    pub admin: Address,
    pub provider: Address,
    pub trader: Address,
    pub underlying: Address,
    pub underlying_asset: StellarAssetClient<'static>,
    pub underlying_tok: TokenClient<'static>,
    pub pt: Address,
    pub pt_asset: StellarAssetClient<'static>,
    pub pt_tok: TokenClient<'static>,
    pub yt: Address,
    pub oracle: Address,
    pub amm: Address,
    pub amm_client: AmmContractClient<'static>,
    pub maturity: u64,
}

impl AmmFixture {
    pub fn new() -> Self {
        Self::with_options(DEFAULT_SCALAR_ROOT, DEFAULT_FEE_BPS, TENOR)
    }

    pub fn with_fee(fee_bps: u32) -> Self {
        Self::with_options(DEFAULT_SCALAR_ROOT, fee_bps, TENOR)
    }

    pub fn with_tenor(tenor: u64) -> Self {
        Self::with_options(DEFAULT_SCALAR_ROOT, DEFAULT_FEE_BPS, tenor)
    }

    pub fn with_options(scalar_root: i128, fee_bps: u32, tenor: u64) -> Self {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|li| {
            li.timestamp = START_TS;
            li.sequence_number = 1;
        });

        let admin = Address::generate(&env);
        let provider = Address::generate(&env);
        let trader = Address::generate(&env);

        // Underlying SAC.
        let und_sac = env.register_stellar_asset_contract_v2(admin.clone());
        let underlying = und_sac.address();
        let underlying_asset = StellarAssetClient::new(&env, &underlying);
        let underlying_tok = TokenClient::new(&env, &underlying);

        // PT SAC — for AMM tests this is a plain SAC, mintable by admin.
        let pt_sac = env.register_stellar_asset_contract_v2(admin.clone());
        let pt = pt_sac.address();
        let pt_asset = StellarAssetClient::new(&env, &pt);
        let pt_tok = TokenClient::new(&env, &pt);

        // YT placeholder (recorded by AMM but never called).
        let yt = Address::generate(&env);
        // Oracle placeholder.
        let oracle = Address::generate(&env);

        let maturity = START_TS + tenor;
        let amm = env.register(
            AmmContract,
            (
                pt.clone(),
                yt.clone(),
                underlying.clone(),
                oracle.clone(),
                maturity,
                scalar_root,
                fee_bps,
                admin.clone(),
            ),
        );
        let amm_client = AmmContractClient::new(&env, &amm);

        Self {
            env,
            admin,
            provider,
            trader,
            underlying,
            underlying_asset,
            underlying_tok,
            pt,
            pt_asset,
            pt_tok,
            yt,
            oracle,
            amm,
            amm_client,
            maturity,
        }
    }

    /// Mint `amount` of underlying to `who`.
    pub fn fund_underlying(&self, who: &Address, amount: i128) {
        self.underlying_asset.mint(who, &amount);
    }

    /// Mint `amount` of PT to `who`.
    pub fn fund_pt(&self, who: &Address, amount: i128) {
        self.pt_asset.mint(who, &amount);
    }

    /// Advance ledger time.
    pub fn advance(&self, dt: u64) {
        let now = self.env.ledger().timestamp();
        self.env.ledger().with_mut(|li| {
            li.timestamp = now + dt;
            li.sequence_number = li.sequence_number.saturating_add(1);
        });
    }

    /// Seed the pool with initial reserves.
    pub fn seed_initial_liquidity(&self, pt_amount: i128, under_amount: i128) -> i128 {
        self.fund_pt(&self.provider, pt_amount);
        self.fund_underlying(&self.provider, under_amount);
        self.amm_client
            .add_liquidity(&self.provider, &pt_amount, &under_amount, &1)
    }
}
