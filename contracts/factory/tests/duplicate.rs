//! Duplicate-market rejection. Covered partly in `determinism.rs`; this
//! file isolates the exact revert path so a single regression in the
//! duplicate-check code is easy to identify.

mod common;

use common::FactoryFixture;

#[test]
#[should_panic]
fn deploy_same_underlying_same_maturity_reverts() {
    let f = FactoryFixture::new();
    let _ = f.deploy_default_market();
    // Second identical call.
    let _ = f.deploy_default_market();
}

#[test]
fn deploy_count_unchanged_after_failed_duplicate() {
    let f = FactoryFixture::new();
    let _ = f.deploy_default_market();
    let count_before = f.factory_client.market_count();
    assert_eq!(count_before, 1);

    // Soroban panic in the contract body rolls storage back fully, so
    // we capture the count by re-querying after the would-be-second
    // call. We do that by running it inside a `try_deploy_market`
    // wrapper — the generated client exposes a `try_*` variant for
    // each entry point that returns `Result`.
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
            &soroban_sdk::String::from_str(&f.env, "PT"),
            &soroban_sdk::String::from_str(&f.env, "PT"),
            &soroban_sdk::String::from_str(&f.env, "YT"),
            &soroban_sdk::String::from_str(&f.env, "YT"),
            &7_u32,
        )
        .is_ok();
    assert!(!ok, "duplicate deploy must revert");
    // Count remains 1 (rollback).
    assert_eq!(f.factory_client.market_count(), 1);
}
