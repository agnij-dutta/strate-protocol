//! Strate Factory — deploys a five-contract market `(Oracle, PT, YT, AMM,
//! YieldStripping)` per `(underlying, maturity)` and tracks the result in a
//! canonical on-chain registry.
//!
//! Per ADR-003. See workspace-level docs for the full design. This file
//! contains:
//!
//! - the five WASM hashes stored at construction,
//! - `deploy_market(...)` which materialises one instance of each contract
//!   from those hashes and wires them together,
//! - an append-only registry: lookup by `(underlying, maturity)` and by
//!   `index`, plus full `MarketMeta` per YS address,
//! - a 48h-timelocked self-upgrade flow (ADR-005). The Factory IS
//!   upgradeable; the five child contracts are NOT.
//!
//! ## Deploy ordering (ADR-001 §4)
//!
//! ```text
//! 1. Oracle (admin, OracleConfig)
//! 2. PT     (PtInitArgs)              # no minter yet
//! 3. YT     (YtInitArgs)              # no minter yet
//! 4. AMM    (pt, yt, underlying, oracle, maturity, scalar_root,
//!            fee_bps, admin)
//! 5. YS     (Config { underlying, pt, yt, oracle, maturity,
//!            scalar_root, admin })
//! 6. PT.initialize_minter(ys)         # one-shot, irreversible
//! 7. YT.initialize_minter(ys)         # one-shot, irreversible
//! ```
//!
//! Steps 6-7 are mandatory: the YS address is only known after step 5,
//! so PT/YT defer their minter bind to a post-construct one-shot init
//! that reverts on a second call (ADR-002 §"Never admin-settable").
//!
//! ## Salt scheme (ADR-003)
//!
//! ```text
//! base_salt = sha256("strate.v1\0" || underlying.to_xdr() ||
//!                    maturity.to_be_bytes())
//! salt_kind = base_salt XOR KIND_TAG_BYTES[kind]
//! ```
//!
//! The XOR mixing keeps the five addresses for one market deterministic
//! AND distinct. ADR-003 explicitly calls for `sha256`, not keccak —
//! Stellar/Soroban-native hash, avoids the Keccak dependency.
//!
//! ## Why this crate depends on the five contract crates
//!
//! Each child contract's `__constructor` argument is a Soroban
//! `contracttype` struct (e.g. `PtInitArgs`, `OracleConfig`). The Soroban
//! codec serialises those as `Map<Symbol, Val>` keyed by field name. To
//! pass the struct across `deploy_v2` we must build the **same struct
//! type** the contract expects. Importing the rlib half of each
//! `cdylib+rlib` crate gives us those types verbatim, with no risk of
//! field-name drift between the constructor and the caller.

#![no_std]
#![deny(unsafe_code)]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::must_use_candidate)]

use soroban_sdk::{
    contract, contractimpl, contracttype, panic_with_error, symbol_short, xdr::ToXdr, Address,
    Bytes, BytesN, Env, IntoVal, Symbol, Val, Vec,
};

use strate_interfaces::Error;

// Constructor arg structs come from the shared types crate. See ADR-001
// and the docstring on packages/strate-types for why the factory cannot
// import from the child contract crates directly.
use strate_types::{OracleConfig, PtInitArgs, YsConfig, YtInitArgs};

// =========================================================================
// Constants
// =========================================================================

/// Persistent TTL bump thresholds. ADR-004 defaults.
const PERSISTENT_BUMP_LOW: u32 = 100_000;
const PERSISTENT_BUMP_HIGH: u32 = 500_000;

/// Minimum tenor a Factory-deployed market must have at construction time.
/// Mirrors ADR-003 §Validation. AMM independently requires
/// `MIN_TOTAL_DURATION_SECONDS + lock_window`, but the Factory enforces
/// the stricter "7 days" rule from ADR-003 so a missed-by-one-second AMM
/// revert can never be reached.
pub const MIN_TENOR_SECONDS: u64 = 7 * 86_400;

/// Maximum tenor — ADR-003 §"Validation": "less than now + 2 years".
pub const MAX_TENOR_SECONDS: u64 = 2 * 365 * 86_400;

/// Hard cap on AMM swap fee (basis points). Mirrors `amm::MAX_FEE_BPS`.
pub const MAX_FEE_BPS: u32 = 100;

/// 48-hour timelock on Factory self-upgrade (ADR-005).
pub const UPGRADE_TIMELOCK_SECONDS: u64 = 48 * 60 * 60;

/// Topic tag for `MarketDeployedEvent`.
const TOPIC_MARKET: Symbol = symbol_short!("market");

/// Topic tag for the Factory pause/unpause events.
const TOPIC_FPAUSE: Symbol = symbol_short!("fpause");

/// Topic tag for upgrade lifecycle events.
const TOPIC_UPGRADE: Symbol = symbol_short!("upgrade");

// =========================================================================
// Bundled args structs
// =========================================================================
//
// Soroban contract entry points are capped at 10 parameters. `deploy_market`
// needs to forward 13 distinct config knobs to the five child contracts, so
// we bundle the oracle config and the PT/YT cosmetic metadata into two
// `contracttype` structs. The wire format is `Map<Symbol, Val>` keyed by
// field name, which the SDK serializes losslessly across the host boundary.

// Both bundle types live in `strate-types`. Re-exported here under the
// historical names so existing call sites (tests, integration harness,
// dApp typings) keep their import paths.
pub use strate_types::{OracleParams, TokenMetadata};

// -- Per-contract salt XOR masks. Constants are intentionally distinct
// byte patterns; their only requirement is pairwise inequality so the
// five derived addresses for one market are distinct. The leading
// ASCII is for visual diff-ability in transactions; the tail is
// zero-padded to 32 bytes.

const ORACLE_SALT_XOR: [u8; 32] = [
    0x4f, 0x52, 0x41, 0x43, 0x4c, 0x45, 0x53, 0x54, // "ORACLEST"
    0x52, 0x41, 0x54, 0x45, 0x76, 0x31, 0x00, 0x01, // "RATEv1\0\x01"
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];
const PT_SALT_XOR: [u8; 32] = [
    0x50, 0x54, 0x54, 0x4f, 0x4b, 0x45, 0x4e, 0x53, // "PTTOKENS"
    0x54, 0x52, 0x41, 0x54, 0x45, 0x76, 0x31, 0x02, // "TRATEv1\x02"
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];
const YT_SALT_XOR: [u8; 32] = [
    0x59, 0x54, 0x54, 0x4f, 0x4b, 0x45, 0x4e, 0x53, // "YTTOKENS"
    0x54, 0x52, 0x41, 0x54, 0x45, 0x76, 0x31, 0x03, // "TRATEv1\x03"
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];
const AMM_SALT_XOR: [u8; 32] = [
    0x41, 0x4d, 0x4d, 0x43, 0x55, 0x52, 0x56, 0x45, // "AMMCURVE"
    0x53, 0x54, 0x52, 0x41, 0x54, 0x45, 0x76, 0x04, // "STRATEv\x04"
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];
const YS_SALT_XOR: [u8; 32] = [
    0x59, 0x53, 0x43, 0x4f, 0x52, 0x45, 0x76, 0x31, // "YSCOREv1"
    0x53, 0x54, 0x52, 0x41, 0x54, 0x45, 0x05, 0x05, // "STRATE\x05\x05"
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];

// =========================================================================
// Types
// =========================================================================

/// `(underlying, maturity)` market key. Used as the discriminant for
/// the `MarketByKey` registry lookup.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketKey {
    pub underlying: Address,
    pub maturity: u64,
}

/// Canonical handle bundle for a deployed market. Returned by
/// `get_market` and emitted as the data payload of a `MarketDeployed`
/// event.
#[contracttype]
#[derive(Clone, Debug)]
pub struct MarketMeta {
    pub pt: Address,
    pub yt: Address,
    pub amm: Address,
    pub oracle: Address,
    pub yield_stripping: Address,
    pub underlying: Address,
    pub maturity: u64,
    pub scalar_root: i128,
    pub fee_bps: u32,
    pub blend_pool: Address,
    pub deployed_at: u64,
    pub deployer: Address,
}

/// Pending self-upgrade proposal (ADR-005). Cleared on execute or
/// cancel.
#[contracttype]
#[derive(Clone, Debug)]
pub struct PendingUpgrade {
    pub new_wasm_hash: BytesN<32>,
    pub eta: u64,
}

#[contracttype]
#[derive(Clone, Debug)]
enum DataKey {
    // ---- Instance (hot config) -----------------------------------------
    Admin,
    Paused,
    OracleWasm,
    PtWasm,
    YtWasm,
    AmmWasm,
    YsWasm,
    PendingUpgrade,

    // ---- Persistent (append-only registry) -----------------------------
    /// `index -> YS address` enumeration.
    MarketByIndex(u32),
    /// `(underlying, maturity) -> YS address` duplicate detection +
    /// canonical lookup.
    MarketByKey(MarketKey),
    /// `YS address -> full MarketMeta`.
    MarketMeta(Address),
    /// Monotonically-increasing count of markets deployed.
    MarketCount,
}

// =========================================================================
// Contract
// =========================================================================

#[contract]
pub struct Factory;

#[contractimpl]
impl Factory {
    /// Initialise the Factory. Each `*_wasm` arg is the 32-byte WASM
    /// hash of the corresponding contract template, uploaded via
    /// `env.deployer().upload_contract_wasm(...)` in a deploy script.
    /// These hashes are immutable for this Factory instance; updating
    /// the templates requires deploying a new Factory.
    pub fn __constructor(
        env: Env,
        admin: Address,
        oracle_wasm: BytesN<32>,
        pt_wasm: BytesN<32>,
        yt_wasm: BytesN<32>,
        amm_wasm: BytesN<32>,
        ys_wasm: BytesN<32>,
    ) {
        let s = env.storage().instance();
        if s.has(&DataKey::Admin) {
            panic_with_error!(&env, Error::AlreadyInitialized);
        }
        s.set(&DataKey::Admin, &admin);
        s.set(&DataKey::Paused, &false);
        s.set(&DataKey::OracleWasm, &oracle_wasm);
        s.set(&DataKey::PtWasm, &pt_wasm);
        s.set(&DataKey::YtWasm, &yt_wasm);
        s.set(&DataKey::AmmWasm, &amm_wasm);
        s.set(&DataKey::YsWasm, &ys_wasm);
        env.storage()
            .persistent()
            .set(&DataKey::MarketCount, &0_u32);
    }

    /// Deploy a fresh five-contract market for `(underlying, maturity)`.
    /// Returns the YS address as the canonical "market handle".
    ///
    /// V1: admin-gated. `deployer` is the operational caller recorded
    /// in `MarketMeta.deployer`; admin auth is required separately.
    /// When v2 flips to permissionless, remove the admin-auth line at
    /// the `PERMISSIONLESS_V2` marker.
    ///
    /// Oracle params (`max_delta_per_sync_wad`, `max_staleness_ledgers`,
    /// `observation_window`) are passed through to the Oracle's
    /// `__constructor`. PT/YT cosmetic metadata is also caller-supplied
    /// so a deploy script can produce vanity names like
    /// "PT-bUSDC-DEC2026".
    #[allow(clippy::too_many_arguments)]
    pub fn deploy_market(
        env: Env,
        deployer: Address,
        underlying: Address,
        blend_pool: Address,
        maturity: u64,
        scalar_root: i128,
        fee_bps: u32,
        oracle_params: OracleParams,
        token_meta: TokenMetadata,
    ) -> Address {
        // Unpack bundled args for the rest of the function body.
        let OracleParams {
            max_delta_per_sync_wad,
            max_staleness_ledgers,
            observation_window,
        } = oracle_params;
        let TokenMetadata {
            pt_name,
            pt_symbol,
            yt_name,
            yt_symbol,
            decimals,
        } = token_meta;
        deployer.require_auth();
        let paused: bool = env
            .storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false);
        if paused {
            panic_with_error!(&env, Error::Paused);
        }

        // PERMISSIONLESS_V2: remove the line below to open deployment.
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));
        admin.require_auth();

        // ---- Validation -----------------------------------------------
        let now = env.ledger().timestamp();
        if maturity <= now {
            panic_with_error!(&env, Error::InvalidMaturity);
        }
        let tenor = maturity - now;
        if tenor < MIN_TENOR_SECONDS || tenor > MAX_TENOR_SECONDS {
            panic_with_error!(&env, Error::InvalidMaturity);
        }
        if scalar_root <= 0 {
            panic_with_error!(&env, Error::Math);
        }
        if fee_bps > MAX_FEE_BPS {
            panic_with_error!(&env, Error::Math);
        }

        // ---- Duplicate check ------------------------------------------
        let market_key = MarketKey {
            underlying: underlying.clone(),
            maturity,
        };
        let key_slot = DataKey::MarketByKey(market_key.clone());
        if env.storage().persistent().has(&key_slot) {
            panic_with_error!(&env, Error::AlreadyInitialized);
        }

        // ---- WASM hashes ----------------------------------------------
        let oracle_wasm: BytesN<32> = env
            .storage()
            .instance()
            .get(&DataKey::OracleWasm)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));
        let pt_wasm: BytesN<32> = env
            .storage()
            .instance()
            .get(&DataKey::PtWasm)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));
        let yt_wasm: BytesN<32> = env
            .storage()
            .instance()
            .get(&DataKey::YtWasm)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));
        let amm_wasm: BytesN<32> = env
            .storage()
            .instance()
            .get(&DataKey::AmmWasm)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));
        let ys_wasm: BytesN<32> = env
            .storage()
            .instance()
            .get(&DataKey::YsWasm)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));

        // ---- Deterministic salts (sha256 base + XOR per kind) ---------
        let base_salt = compute_base_salt(&env, &underlying, maturity);
        let salt_oracle = xor_salt(&env, &base_salt, &ORACLE_SALT_XOR);
        let salt_pt = xor_salt(&env, &base_salt, &PT_SALT_XOR);
        let salt_yt = xor_salt(&env, &base_salt, &YT_SALT_XOR);
        let salt_amm = xor_salt(&env, &base_salt, &AMM_SALT_XOR);
        let salt_ys = xor_salt(&env, &base_salt, &YS_SALT_XOR);

        let factory_addr = env.current_contract_address();

        // ===============================================================
        // DEPLOY ORDER (ADR-001 §4): Oracle -> PT -> YT -> AMM -> YS
        // ===============================================================

        // 1. Oracle
        let oracle_cfg = OracleConfig {
            blend_pool: blend_pool.clone(),
            max_delta_per_sync_wad,
            max_staleness_ledgers,
            observation_window,
        };
        let oracle_args: Vec<Val> = (admin.clone(), oracle_cfg).into_val(&env);
        let oracle = env
            .deployer()
            .with_address(factory_addr.clone(), salt_oracle)
            .deploy_v2(oracle_wasm, oracle_args);

        // 2. PT
        let pt_args_struct = PtInitArgs {
            underlying: underlying.clone(),
            maturity,
            name: pt_name,
            symbol: pt_symbol,
            decimals,
        };
        let pt_args: Vec<Val> = (pt_args_struct,).into_val(&env);
        let pt = env
            .deployer()
            .with_address(factory_addr.clone(), salt_pt)
            .deploy_v2(pt_wasm, pt_args);

        // 3. YT
        let yt_args_struct = YtInitArgs {
            underlying: underlying.clone(),
            maturity,
            name: yt_name,
            symbol: yt_symbol,
            decimals,
        };
        let yt_args: Vec<Val> = (yt_args_struct,).into_val(&env);
        let yt = env
            .deployer()
            .with_address(factory_addr.clone(), salt_yt)
            .deploy_v2(yt_wasm, yt_args);

        // 4. AMM — positional 8-arg signature.
        // AmmContract::__constructor(pt, yt, underlying, oracle, maturity,
        // scalar_root, fee_bps, admin)
        let amm_args: Vec<Val> = (
            pt.clone(),
            yt.clone(),
            underlying.clone(),
            oracle.clone(),
            maturity,
            scalar_root,
            fee_bps,
            admin.clone(),
        )
            .into_val(&env);
        let amm = env
            .deployer()
            .with_address(factory_addr.clone(), salt_amm)
            .deploy_v2(amm_wasm, amm_args);

        // 5. YieldStripping
        let ys_cfg = YsConfig {
            underlying: underlying.clone(),
            pt: pt.clone(),
            yt: yt.clone(),
            oracle: oracle.clone(),
            maturity,
            scalar_root,
            admin: admin.clone(),
        };
        let ys_args: Vec<Val> = (ys_cfg,).into_val(&env);
        let ys = env
            .deployer()
            .with_address(factory_addr.clone(), salt_ys)
            .deploy_v2(ys_wasm, ys_args);

        // ===============================================================
        // ONE-SHOT initialize_minter on BOTH PT and YT — must happen
        // AFTER YS deploy because the YS address IS the minter.
        // (ADR-001 §4 + ADR-002 §"Never admin-settable")
        // ===============================================================
        let init_minter_sym = Symbol::new(&env, "initialize_minter");
        let _: Val = env.invoke_contract(&pt, &init_minter_sym, (ys.clone(),).into_val(&env));
        let _: Val = env.invoke_contract(&yt, &init_minter_sym, (ys.clone(),).into_val(&env));

        // ===============================================================
        // REGISTER (append-only)
        // ===============================================================
        let index: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::MarketCount)
            .unwrap_or(0);
        let meta = MarketMeta {
            pt: pt.clone(),
            yt: yt.clone(),
            amm: amm.clone(),
            oracle: oracle.clone(),
            yield_stripping: ys.clone(),
            underlying: underlying.clone(),
            maturity,
            scalar_root,
            fee_bps,
            blend_pool: blend_pool.clone(),
            deployed_at: now,
            deployer: deployer.clone(),
        };

        let by_idx_key = DataKey::MarketByIndex(index);
        env.storage().persistent().set(&by_idx_key, &ys);
        env.storage().persistent().extend_ttl(
            &by_idx_key,
            PERSISTENT_BUMP_LOW,
            PERSISTENT_BUMP_HIGH,
        );

        env.storage().persistent().set(&key_slot, &ys);
        env.storage()
            .persistent()
            .extend_ttl(&key_slot, PERSISTENT_BUMP_LOW, PERSISTENT_BUMP_HIGH);

        let meta_key = DataKey::MarketMeta(ys.clone());
        env.storage().persistent().set(&meta_key, &meta);
        env.storage().persistent().extend_ttl(
            &meta_key,
            PERSISTENT_BUMP_LOW,
            PERSISTENT_BUMP_HIGH,
        );

        env.storage()
            .persistent()
            .set(&DataKey::MarketCount, &(index + 1));

        env.events()
            .publish((TOPIC_MARKET, underlying.clone(), maturity), meta);

        ys
    }

    // ---------------------------------------------------------------------
    // Read-only views
    // ---------------------------------------------------------------------

    /// Lookup the canonical YS address for `(underlying, maturity)`.
    pub fn market_address(env: Env, underlying: Address, maturity: u64) -> Option<Address> {
        let key = MarketKey { underlying, maturity };
        env.storage().persistent().get(&DataKey::MarketByKey(key))
    }

    /// Lookup the full `MarketMeta` for `(underlying, maturity)`.
    pub fn get_market(env: Env, underlying: Address, maturity: u64) -> Option<MarketMeta> {
        let ys = Self::market_address(env.clone(), underlying, maturity)?;
        env.storage().persistent().get(&DataKey::MarketMeta(ys))
    }

    /// Lookup the YS address by registry index (`0..market_count()`).
    pub fn get_market_by_index(env: Env, index: u32) -> Option<Address> {
        env.storage()
            .persistent()
            .get(&DataKey::MarketByIndex(index))
    }

    /// Lookup the full `MarketMeta` by YS address.
    pub fn get_market_meta(env: Env, ys: Address) -> Option<MarketMeta> {
        env.storage().persistent().get(&DataKey::MarketMeta(ys))
    }

    /// Number of markets ever deployed.
    pub fn market_count(env: Env) -> u32 {
        env.storage()
            .persistent()
            .get(&DataKey::MarketCount)
            .unwrap_or(0)
    }

    /// Pending self-upgrade proposal, if any.
    pub fn pending_upgrade(env: Env) -> Option<PendingUpgrade> {
        env.storage().instance().get(&DataKey::PendingUpgrade)
    }

    pub fn admin(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized))
    }

    pub fn is_paused(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false)
    }

    // ---------------------------------------------------------------------
    // Admin: pause / unpause
    // ---------------------------------------------------------------------

    /// Pause the Factory. Halts `deploy_market`. Existing markets keep
    /// running — they have their own pause surface.
    pub fn pause(env: Env) {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));
        admin.require_auth();
        env.storage().instance().set(&DataKey::Paused, &true);
        env.events()
            .publish((TOPIC_FPAUSE, symbol_short!("on")), ());
    }

    /// Unpause the Factory. Admin-only. The Factory pause only blocks
    /// new-market deployment, so there is no user-facing griefing
    /// surface that warrants a 48h timelock (unlike the per-market
    /// contracts, which use asymmetric unpause per ADR-002).
    pub fn unpause(env: Env) {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));
        admin.require_auth();
        env.storage().instance().set(&DataKey::Paused, &false);
        env.events()
            .publish((TOPIC_FPAUSE, symbol_short!("off")), ());
    }

    // ---------------------------------------------------------------------
    // Upgrade flow (ADR-005, 48h timelock)
    // ---------------------------------------------------------------------

    /// Propose a Factory self-upgrade. Records `(new_wasm_hash, eta =
    /// now + 48h)`. Only one proposal can be open at a time; calling
    /// this again overwrites the pending proposal and resets the eta.
    pub fn propose_upgrade(env: Env, new_wasm_hash: BytesN<32>) {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));
        admin.require_auth();
        let eta = env
            .ledger()
            .timestamp()
            .saturating_add(UPGRADE_TIMELOCK_SECONDS);
        let pending = PendingUpgrade {
            new_wasm_hash,
            eta,
        };
        env.storage()
            .instance()
            .set(&DataKey::PendingUpgrade, &pending);
        env.events()
            .publish((TOPIC_UPGRADE, symbol_short!("propose")), eta);
    }

    /// Execute a previously-proposed upgrade. Permissionless after eta
    /// — anyone can prod, but the admin must have proposed. The
    /// `new_wasm_hash` argument must match the pending proposal
    /// exactly (defensive double-check against host-state confusion).
    pub fn execute_upgrade(env: Env, new_wasm_hash: BytesN<32>) {
        let pending: PendingUpgrade = env
            .storage()
            .instance()
            .get(&DataKey::PendingUpgrade)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));
        if pending.new_wasm_hash != new_wasm_hash {
            panic_with_error!(&env, Error::Unauthorized);
        }
        if env.ledger().timestamp() < pending.eta {
            panic_with_error!(&env, Error::Unauthorized);
        }
        env.deployer().update_current_contract_wasm(new_wasm_hash);
        env.storage().instance().remove(&DataKey::PendingUpgrade);
        env.events()
            .publish((TOPIC_UPGRADE, symbol_short!("exec")), ());
    }

    /// Cancel an open upgrade proposal. Admin-only.
    pub fn cancel_upgrade(env: Env) {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));
        admin.require_auth();
        env.storage().instance().remove(&DataKey::PendingUpgrade);
        env.events()
            .publish((TOPIC_UPGRADE, symbol_short!("cancel")), ());
    }
}

// =========================================================================
// Helpers
// =========================================================================

/// `base_salt = sha256("strate.v1\0" || underlying.to_xdr() ||
///                     maturity.to_be_bytes())`, per ADR-003.
///
/// Note we deliberately use `sha256` (Stellar/Soroban-native) rather
/// than `keccak256` — see ADR-003 §"Address derivation". The literal
/// ADR sentence is reproduced here so a future contributor can't
/// "fix" the algorithm out of EVM-muscle-memory:
///
/// > Note: we use `sha256` rather than `keccak256` because it's the
/// > Stellar/Soroban-native hash and avoids importing a Keccak
/// > dependency. Functionally equivalent for salt purposes.
fn compute_base_salt(env: &Env, underlying: &Address, maturity: u64) -> BytesN<32> {
    let mut buf = Bytes::new(env);
    buf.append(&Bytes::from_slice(env, b"strate.v1\0"));
    buf.append(&underlying.clone().to_xdr(env));
    buf.append(&Bytes::from_slice(env, &maturity.to_be_bytes()));
    env.crypto().sha256(&buf).into()
}

/// XOR a base salt with a per-contract 32-byte mask. Pure function —
/// derives a unique-but-deterministic salt per child contract from the
/// shared `base_salt`.
fn xor_salt(env: &Env, base: &BytesN<32>, mask: &[u8; 32]) -> BytesN<32> {
    let base_arr = base.to_array();
    let mut out = [0_u8; 32];
    let mut i = 0;
    while i < 32 {
        out[i] = base_arr[i] ^ mask[i];
        i += 1;
    }
    BytesN::from_array(env, &out)
}
