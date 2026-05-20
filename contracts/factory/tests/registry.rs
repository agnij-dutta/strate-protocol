//! Registry surface tests: count, by-index, by-key, by-meta.

mod common;

use common::{FactoryFixture, DAY, START_TS};

#[test]
fn market_count_increments_per_deploy() {
    let f = FactoryFixture::new();
    assert_eq!(f.factory_client.market_count(), 0);
    let _ = f.deploy_default_market();
    assert_eq!(f.factory_client.market_count(), 1);

    // Deploy a second with a different maturity.
    let m2 = START_TS + 180 * DAY;
    let deployer = f.admin.clone();
    let _ = f.factory_client.deploy_market(
        &deployer,
        &f.underlying,
        &f.blend_pool,
        &m2,
        &common::WAD,
        &30_u32,
        &50_000_000_000_000_000_i128,
        &120_u32,
        &32_u32,
        &soroban_sdk::String::from_str(&f.env, "PT-180"),
        &soroban_sdk::String::from_str(&f.env, "PT-180"),
        &soroban_sdk::String::from_str(&f.env, "YT-180"),
        &soroban_sdk::String::from_str(&f.env, "YT-180"),
        &7_u32,
    );
    assert_eq!(f.factory_client.market_count(), 2);
}

#[test]
fn get_market_by_index_returns_each_address() {
    let f = FactoryFixture::new();
    let ys1 = f.deploy_default_market();

    let m2 = START_TS + 180 * DAY;
    let deployer = f.admin.clone();
    let ys2 = f.factory_client.deploy_market(
        &deployer,
        &f.underlying,
        &f.blend_pool,
        &m2,
        &common::WAD,
        &30_u32,
        &50_000_000_000_000_000_i128,
        &120_u32,
        &32_u32,
        &soroban_sdk::String::from_str(&f.env, "PT-180"),
        &soroban_sdk::String::from_str(&f.env, "PT-180"),
        &soroban_sdk::String::from_str(&f.env, "YT-180"),
        &soroban_sdk::String::from_str(&f.env, "YT-180"),
        &7_u32,
    );

    assert_eq!(f.factory_client.get_market_by_index(&0).unwrap(), ys1);
    assert_eq!(f.factory_client.get_market_by_index(&1).unwrap(), ys2);
    assert!(f.factory_client.get_market_by_index(&2).is_none());
}

#[test]
fn get_market_returns_full_meta() {
    let f = FactoryFixture::new();
    let ys = f.deploy_default_market();
    let maturity = START_TS + common::TENOR;
    let meta = f
        .factory_client
        .get_market(&f.underlying, &maturity)
        .unwrap();
    assert_eq!(meta.yield_stripping, ys);
    assert_eq!(meta.maturity, maturity);
    assert_eq!(meta.deployed_at, START_TS);
    assert_eq!(meta.blend_pool, f.blend_pool);

    // by_meta returns the same struct.
    let meta2 = f.factory_client.get_market_meta(&ys).unwrap();
    assert_eq!(meta2.maturity, meta.maturity);
}

#[test]
fn unknown_market_returns_none() {
    let f = FactoryFixture::new();
    let made_up_maturity = START_TS + 999 * DAY;
    assert!(f
        .factory_client
        .get_market(&f.underlying, &made_up_maturity)
        .is_none());
    assert!(f
        .factory_client
        .market_address(&f.underlying, &made_up_maturity)
        .is_none());
}
