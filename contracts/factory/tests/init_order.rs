//! Verifies that after `deploy_market`, the PT and YT contracts have
//! their `minter` field set to the **YS** address (not zero, not the
//! admin, not the Factory).
//!
//! This is the linchpin of ADR-001 §4: PT/YT mint/burn must only be
//! callable by YS. If `initialize_minter` were skipped or pointed at
//! the wrong address, YS's mint/burn flow would revert post-deploy and
//! the entire protocol would be inert.

mod common;

use common::FactoryFixture;
use pt_token::PtTokenClient;
use yt_token::YtTokenClient;

#[test]
fn pt_and_yt_minter_is_ys_address() {
    let f = FactoryFixture::new();
    let ys = f.deploy_default_market();
    let maturity = common::START_TS + common::TENOR;

    let meta = f
        .factory_client
        .get_market(&f.underlying, &maturity)
        .unwrap();

    let pt_minter = PtTokenClient::new(&f.env, &meta.pt).minter();
    let yt_minter = YtTokenClient::new(&f.env, &meta.yt).minter();

    assert_eq!(pt_minter, ys, "PT.minter must be the YS address");
    assert_eq!(yt_minter, ys, "YT.minter must be the YS address");

    // Sanity: minter is NOT the admin (a common copy-paste failure
    // mode in factory implementations) and NOT the Factory address
    // itself.
    assert_ne!(pt_minter, f.admin);
    assert_ne!(yt_minter, f.admin);
    assert_ne!(pt_minter, f.factory);
    assert_ne!(yt_minter, f.factory);
}

#[test]
#[should_panic]
fn pt_initialize_minter_is_one_shot() {
    // The Factory wires the minter once. Any attempt to re-initialize
    // (even by the Factory) must revert with AlreadyInitialized.
    let f = FactoryFixture::new();
    let ys = f.deploy_default_market();
    let maturity = common::START_TS + common::TENOR;
    let meta = f
        .factory_client
        .get_market(&f.underlying, &maturity)
        .unwrap();

    // Direct call against the PT contract — should revert.
    PtTokenClient::new(&f.env, &meta.pt).initialize_minter(&ys);
}
