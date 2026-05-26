//! Factory pause / unpause tests.
//!
//! ADR-005: while the Factory is paused, `deploy_market` reverts.
//! Per-market contracts (which were already deployed) are unaffected.

mod common;

use common::FactoryFixture;

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
            &common::default_oracle_params(),
            &common::make_token_meta(&f.env, "PT", "PT", "YT", "YT"),
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
