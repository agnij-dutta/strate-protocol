//! Determinism + duplicate-rejection tests.
//!
//! ADR-003 requires deterministic addresses derived from
//! `sha256(underlying || maturity)`. We verify two properties:
//!
//! 1. Deploying twice with the same `(underlying, maturity)` reverts
//!    (the second call hits the duplicate guard before the WASM hash
//!    collision matters).
//! 2. Even after the maturity has passed, re-deploy at the same key
//!    still reverts — markets are append-only and historical PT
//!    redemption rights stay intact.

mod common;

use common::{FactoryFixture, DAY, START_TS, TENOR};

#[test]
#[should_panic]
fn redeploying_same_key_reverts() {
    let f = FactoryFixture::new();
    let _ys = f.deploy_default_market();
    // Second call with identical args must revert (MarketExists).
    let _ys2 = f.deploy_default_market();
}

#[test]
#[should_panic]
fn redeploy_after_maturity_still_reverts() {
    let f = FactoryFixture::new();
    let _ys = f.deploy_default_market();

    // Fast-forward past maturity + a buffer.
    f.advance(TENOR + DAY);

    // Even though the maturity has elapsed, the registry retains the
    // entry — markets are append-only per ADR-003.
    let _ys2 = f.deploy_default_market();
}

#[test]
fn distinct_maturities_succeed() {
    let f = FactoryFixture::new();
    let _ys1 = f.deploy_default_market();
    let count_after_first = f.factory_client.market_count();
    assert_eq!(count_after_first, 1);

    // Deploy a second market with a different maturity. Use the public
    // deploy_market entrypoint directly so we control maturity.
    let maturity2 = START_TS + 180 * DAY;
    let deployer = f.admin.clone();
    let _ys2 = f.factory_client.deploy_market(
        &deployer,
        &f.underlying,
        &f.blend_pool,
        &maturity2,
        &common::WAD,
        &30_u32,
        &0_i128,
        &common::default_oracle_params(),
        &common::make_token_meta(
            &f.env,
            "PT-bUSDC-180d",
            "PT-bUSDC",
            "YT-bUSDC-180d",
            "YT-bUSDC",
        ),
    );
    assert_eq!(f.factory_client.market_count(), 2);
}
