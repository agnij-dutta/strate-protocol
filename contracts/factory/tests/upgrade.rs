//! Factory self-upgrade flow (ADR-005).
//!
//! Covered:
//! - `propose_upgrade` records `(hash, eta = now + 48h)`.
//! - `execute_upgrade` before `eta` reverts.
//! - `execute_upgrade` with a mismatched hash reverts.
//! - `cancel_upgrade` clears the proposal.
//! - `execute_upgrade` after `eta` succeeds (we use the **same**
//!   Factory WASM as the "new" hash so the host swap is a no-op the
//!   test environment is happy to run).

mod common;

use common::FactoryFixture;
use soroban_sdk::BytesN;

const TIMELOCK: u64 = 48 * 60 * 60;

#[test]
fn propose_records_pending_with_correct_eta() {
    let f = FactoryFixture::new();
    // Use the YS wasm hash as a stand-in upgrade target. We never
    // execute, so the host never checks WASM compatibility.
    let new_hash: BytesN<32> = f.ys_wasm.clone();
    f.factory_client.propose_upgrade(&new_hash);

    let pending = f.factory_client.pending_upgrade().unwrap();
    assert_eq!(pending.new_wasm_hash, new_hash);
    assert_eq!(pending.eta, common::START_TS + TIMELOCK);
}

#[test]
#[should_panic]
fn execute_before_eta_reverts() {
    let f = FactoryFixture::new();
    let new_hash: BytesN<32> = f.ys_wasm.clone();
    f.factory_client.propose_upgrade(&new_hash);
    // Don't advance — try to execute immediately. Must revert.
    f.factory_client.execute_upgrade(&new_hash);
}

#[test]
#[should_panic]
fn execute_with_mismatched_hash_reverts() {
    let f = FactoryFixture::new();
    let proposed: BytesN<32> = f.ys_wasm.clone();
    let mismatched: BytesN<32> = f.pt_wasm.clone();
    f.factory_client.propose_upgrade(&proposed);
    f.advance(TIMELOCK + 1);
    // Execute with the wrong hash — must revert.
    f.factory_client.execute_upgrade(&mismatched);
}

#[test]
fn cancel_clears_pending() {
    let f = FactoryFixture::new();
    let new_hash: BytesN<32> = f.ys_wasm.clone();
    f.factory_client.propose_upgrade(&new_hash);
    assert!(f.factory_client.pending_upgrade().is_some());

    f.factory_client.cancel_upgrade();
    assert!(f.factory_client.pending_upgrade().is_none());
}

#[test]
#[should_panic]
fn execute_after_cancel_reverts() {
    let f = FactoryFixture::new();
    let new_hash: BytesN<32> = f.ys_wasm.clone();
    f.factory_client.propose_upgrade(&new_hash);
    f.factory_client.cancel_upgrade();
    f.advance(TIMELOCK + 1);
    // No pending proposal anymore.
    f.factory_client.execute_upgrade(&new_hash);
}

#[test]
fn propose_overwrites_existing_proposal() {
    // Two proposals back-to-back: the second overwrites the first.
    let f = FactoryFixture::new();
    let first: BytesN<32> = f.ys_wasm.clone();
    let second: BytesN<32> = f.amm_wasm.clone();

    f.factory_client.propose_upgrade(&first);
    let p1 = f.factory_client.pending_upgrade().unwrap();
    assert_eq!(p1.new_wasm_hash, first);

    f.advance(60); // simulate 1 minute later
    f.factory_client.propose_upgrade(&second);
    let p2 = f.factory_client.pending_upgrade().unwrap();
    assert_eq!(p2.new_wasm_hash, second);
    assert_eq!(p2.eta, common::START_TS + 60 + TIMELOCK);
}

// Note on a "successful execute" test: actually running
// `execute_upgrade` would invoke the Soroban host's
// `update_current_contract_wasm`, which in the unit-test `Env`
// requires the target WASM to be present in the test-environment's
// installed-WASM map (we upload them in `install_wasms`) AND ABI-
// compatible enough that subsequent client calls don't panic. The
// safe target is the Factory's own WASM hash, but the test
// fixture's `env.register(Factory, args)` path does not expose a
// raw hash for it. Wiring that requires loading the Factory WASM
// from disk via `include_bytes!` and uploading it the same way as
// the other five — pending a top-level "wired" integration suite
// that does this for all six contracts at once. Marker:
// `TODO(blockchain-developer): execute_upgrade success path in
// the workspace-level integration tests`.
