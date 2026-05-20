//! Factory pause / unpause tests.
//!
//! ADR-005: while the Factory is paused, `deploy_market` reverts.
//! Per-market contracts (which were already deployed) are unaffected.

mod common;

use common::FactoryFixture;
use soroban_sdk::String;

#[test]
fn paused_factory_rejects_deploy_market() {
    let f = FactoryFixture::new();
    f.factory_client.pause();
    assert!(f.factory_client.is_paused());

    let deployer = f.admin.clone();
    let maturity = common::START_TS + common::TENOR;
    let ok = f
        .factory_client
        .try_deploy_market(
            &deployer,
            &f.underlying,
            &f.blend_pool,
            &maturity,
            &common::WAD,
            &30_u32,
            &50_000_000_000_000_000_i128,
            &120_u32,
            &32_u32,
            &String::from_str(&f.env, "PT"),
            &String::from_str(&f.env, "PT"),
            &String::from_str(&f.env, "YT"),
            &String::from_str(&f.env, "YT"),
            &7_u32,
        )
        .is_ok();
    assert!(!ok, "paused factory must reject deploy_market");
}

#[test]
fn unpause_re_enables_deploy() {
    let f = FactoryFixture::new();
    f.factory_client.pause();
    assert!(f.factory_client.is_paused());
    f.factory_client.unpause();
    assert!(!f.factory_client.is_paused());

    // Now deploy succeeds.
    let _ = f.deploy_default_market();
    assert_eq!(f.factory_client.market_count(), 1);
}
