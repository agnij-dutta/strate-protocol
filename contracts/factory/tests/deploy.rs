//! Happy-path Factory deploy test.
//!
//! Verifies that a fresh `deploy_market` returns a YS address, registers
//! all 5 contract addresses in the registry, and that they are
//! pairwise distinct.

mod common;

use common::FactoryFixture;

#[test]
fn deploy_happy_path_returns_ys_and_registers_market() {
    let f = FactoryFixture::new();
    let ys = f.deploy_default_market();

    // Registry: market_count is 1.
    assert_eq!(f.factory_client.market_count(), 1);

    // Lookup by index returns the YS address.
    let idx0 = f.factory_client.get_market_by_index(&0).unwrap();
    assert_eq!(idx0, ys);

    // Lookup by (underlying, maturity) returns the YS address.
    let maturity = common::START_TS + common::TENOR;
    let by_key = f
        .factory_client
        .market_address(&f.underlying, &maturity)
        .unwrap();
    assert_eq!(by_key, ys);

    // Full meta lookup.
    let meta = f.factory_client.get_market_meta(&ys).unwrap();
    assert_eq!(meta.yield_stripping, ys);
    assert_eq!(meta.underlying, f.underlying);
    assert_eq!(meta.maturity, maturity);
    assert_eq!(meta.scalar_root, common::WAD);
    assert_eq!(meta.fee_bps, 30);
    assert_eq!(meta.deployer, f.admin);

    // Five addresses are pairwise distinct.
    let addrs = [
        &meta.pt,
        &meta.yt,
        &meta.amm,
        &meta.oracle,
        &meta.yield_stripping,
    ];
    for i in 0..5 {
        for j in (i + 1)..5 {
            assert_ne!(
                addrs[i], addrs[j],
                "addresses must be pairwise distinct (i={i}, j={j})",
            );
        }
    }
}

#[test]
fn deploy_returns_full_meta_via_get_market() {
    let f = FactoryFixture::new();
    let ys = f.deploy_default_market();
    let maturity = common::START_TS + common::TENOR;
    let meta = f
        .factory_client
        .get_market(&f.underlying, &maturity)
        .unwrap();
    assert_eq!(meta.yield_stripping, ys);
    assert_eq!(meta.blend_pool, f.blend_pool);
}
