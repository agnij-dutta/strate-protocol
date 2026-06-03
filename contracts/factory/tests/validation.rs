//! Constructor-arg validation: maturity, scalar_root, fee_bps.

mod common;

use common::{FactoryFixture, DAY, START_TS, WAD};

const SHORT_MATURITY: u64 = START_TS + 5 * DAY;

fn try_deploy(f: &FactoryFixture, maturity: u64, scalar_root: i128, fee_bps: u32) -> bool {
    let deployer = f.admin.clone();
    f.factory_client
        .try_deploy_market(
            &deployer,
            &f.underlying,
            &f.blend_pool,
            &maturity,
            &scalar_root,
            &fee_bps,
            &common::default_oracle_params(),
            &common::make_token_meta(&f.env, "PT", "PT", "YT", "YT"),
        )
        .is_ok()
}

#[test]
fn maturity_in_past_reverts() {
    let f = FactoryFixture::new();
    let bad = START_TS - 1;
    assert!(!try_deploy(&f, bad, WAD, 30));
}

#[test]
fn maturity_at_now_reverts() {
    let f = FactoryFixture::new();
    let bad = START_TS;
    assert!(!try_deploy(&f, bad, WAD, 30));
}

#[test]
fn maturity_below_min_tenor_reverts() {
    let f = FactoryFixture::new();
    let bad = SHORT_MATURITY;
    assert!(!try_deploy(&f, bad, WAD, 30));
}

#[test]
fn maturity_above_max_tenor_reverts() {
    let f = FactoryFixture::new();
    let bad = START_TS + 3 * 365 * DAY; // 3 years > 2-year cap
    assert!(!try_deploy(&f, bad, WAD, 30));
}

#[test]
fn zero_scalar_root_reverts() {
    let f = FactoryFixture::new();
    let maturity = START_TS + common::TENOR;
    assert!(!try_deploy(&f, maturity, 0, 30));
}

#[test]
fn negative_scalar_root_reverts() {
    let f = FactoryFixture::new();
    let maturity = START_TS + common::TENOR;
    assert!(!try_deploy(&f, maturity, -1, 30));
}

#[test]
fn fee_above_cap_reverts() {
    let f = FactoryFixture::new();
    let maturity = START_TS + common::TENOR;
    assert!(!try_deploy(&f, maturity, WAD, 101));
}

#[test]
fn fee_at_cap_succeeds() {
    let f = FactoryFixture::new();
    let maturity = START_TS + common::TENOR;
    assert!(try_deploy(&f, maturity, WAD, 100));
}
