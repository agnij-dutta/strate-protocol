//! Strate PT — Principal Token (SEP-41).
//!
//! A SEP-41 fungible token whose mint and burn surface is gated by a
//! single, irrevocable `minter` address — the parent `YieldStripping`
//! contract that issued the market. There is no admin role beyond that
//! authority.
//!
//! Per ADR-002 ("Immutability and Admin Surface"):
//!   - `minter`     — set exactly once via `initialize_minter`, never changes.
//!   - `underlying` — display-only handle to the wrapped asset.
//!   - `maturity`   — Unix timestamp at which 1 PT redeems for 1 underlying.
//!
//! Storage: Instance for the (small) config tuple; Persistent for the
//! per-account balances and allowances, per ADR-004. State-mutating
//! entry points bump the persistent TTL of the keys they touch.
//!
//! ## Why no OpenZeppelin dependency?
//!
//! The original dispatch envisaged the use of `stellar-fungible::Base`,
//! but the public 0.4 API surface for that crate is in flux (the trait
//! re-exports were renamed twice between 0.3 and 0.4). Rather than
//! pinning to an unstable path, we ship a tiny first-party SEP-41
//! implementation here — ~150 lines, deterministic, auditable in
//! isolation. The semantics are identical to the OZ base.

#![no_std]
#![deny(unsafe_code)]

use soroban_sdk::{
    contract, contractimpl, contracttype, panic_with_error, symbol_short, Address, Env, String,
};

use strate_interfaces::Error;

// -------------------------------------------------------------------------
// Storage layout
// -------------------------------------------------------------------------

/// Persistent TTL extension target, in ledgers (~30 days @ 5s/ledger).
/// Mirrors the constant in ADR-004 §"TTL bump strategy". Hard-coded here
/// because PT has no admin and therefore no setter.
const EXTENDED_TTL: u32 = 500_000;
/// Bump threshold — only refresh TTL if remaining is under this.
const TTL_BUMP_THRESHOLD: u32 = 100_000;

#[contracttype]
#[derive(Clone)]
enum DataKey {
    /// Set once by `initialize_minter`. The YieldStripping address.
    Minter,
    /// Display handle for the wrapped asset.
    Underlying,
    /// Unix seconds.
    Maturity,
    /// SEP-41 metadata: `name`, `symbol`, `decimals`.
    Name,
    Symbol,
    Decimals,
    /// Aggregate supply (instance — read on every mint/burn).
    TotalSupply,
    /// Per-holder balance (persistent).
    Balance(Address),
    /// Per-pair allowance (persistent).
    Allowance(Address, Address),
}

// -------------------------------------------------------------------------
// Constructor args
// -------------------------------------------------------------------------

/// Constructor input. The factory passes this when deploying the token.
/// Re-exported from [`strate_types::PtInitArgs`] so the wire format is
/// shared with the Factory (which depends only on `strate-types`, not on
/// this crate's cdylib half).
pub use strate_types::PtInitArgs;

// -------------------------------------------------------------------------
// Contract
// -------------------------------------------------------------------------

/// The PT token.
#[contract]
pub struct PtToken;

#[contractimpl]
impl PtToken {
    /// Construct.
    ///
    /// Note: `__constructor` (no double-underscore in Soroban 22) runs
    /// automatically at deploy. We store the immutable display/metadata
    /// fields here. The `minter` is set in a follow-up call from the
    /// Factory (or YieldStripping) via [`Self::initialize_minter`].
    pub fn __constructor(env: Env, args: PtInitArgs) {
        let storage = env.storage().instance();
        storage.set(&DataKey::Underlying, &args.underlying);
        storage.set(&DataKey::Maturity, &args.maturity);
        storage.set(&DataKey::Name, &args.name);
        storage.set(&DataKey::Symbol, &args.symbol);
        storage.set(&DataKey::Decimals, &args.decimals);
        storage.set(&DataKey::TotalSupply, &0_i128);
    }

    // ---------------------------------------------------------------------
    // One-shot minter bootstrap
    // ---------------------------------------------------------------------

    /// Set the minter exactly once. The factory calls this immediately
    /// after deployment with the address of the parent `YieldStripping`
    /// contract. Subsequent calls revert with `AlreadyInitialized`.
    ///
    /// No `require_auth` here — calling this address is the bootstrap
    /// privilege. In production deployments the factory wraps the entire
    /// deploy + init pair in one transaction so no other principal can
    /// race the call.
    pub fn initialize_minter(env: Env, minter: Address) {
        let storage = env.storage().instance();
        if storage.has(&DataKey::Minter) {
            panic_with_error!(&env, Error::AlreadyInitialized);
        }
        storage.set(&DataKey::Minter, &minter);
    }

    // ---------------------------------------------------------------------
    // SEP-41 metadata
    // ---------------------------------------------------------------------

    pub fn name(env: Env) -> String {
        env.storage()
            .instance()
            .get(&DataKey::Name)
            .unwrap_or_else(|| String::from_str(&env, ""))
    }

    pub fn symbol(env: Env) -> String {
        env.storage()
            .instance()
            .get(&DataKey::Symbol)
            .unwrap_or_else(|| String::from_str(&env, ""))
    }

    pub fn decimals(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::Decimals)
            .unwrap_or(7)
    }

    /// Maturity timestamp (Unix seconds). Display-only.
    pub fn maturity(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&DataKey::Maturity)
            .unwrap_or(0)
    }

    /// The minter address, or panics if not yet initialized.
    pub fn minter(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::Minter)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized))
    }

    /// Total supply.
    pub fn total_supply(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&DataKey::TotalSupply)
            .unwrap_or(0)
    }

    // ---------------------------------------------------------------------
    // SEP-41 balance / transfer / allowance
    // ---------------------------------------------------------------------

    pub fn balance(env: Env, id: Address) -> i128 {
        read_balance(&env, &id)
    }

    pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
        from.require_auth();
        check_nonneg(&env, amount);
        do_transfer(&env, &from, &to, amount);
        emit_transfer(&env, &from, &to, amount);
    }

    pub fn transfer_from(env: Env, spender: Address, from: Address, to: Address, amount: i128) {
        spender.require_auth();
        check_nonneg(&env, amount);
        consume_allowance(&env, &from, &spender, amount);
        do_transfer(&env, &from, &to, amount);
        emit_transfer(&env, &from, &to, amount);
    }

    pub fn approve(
        env: Env,
        from: Address,
        spender: Address,
        amount: i128,
        expiration_ledger: u32,
    ) {
        from.require_auth();
        check_nonneg(&env, amount);
        let key = DataKey::Allowance(from.clone(), spender.clone());
        env.storage().persistent().set(&key, &amount);
        if amount > 0 {
            env.storage().persistent().extend_ttl(
                &key,
                expiration_ledger.min(EXTENDED_TTL),
                expiration_ledger.min(EXTENDED_TTL),
            );
        }
        env.events().publish(
            (symbol_short!("approve"), from.clone(), spender.clone()),
            amount,
        );
    }

    pub fn allowance(env: Env, from: Address, spender: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&DataKey::Allowance(from, spender))
            .unwrap_or(0)
    }

    // ---------------------------------------------------------------------
    // Privileged mint / burn (minter-only)
    // ---------------------------------------------------------------------

    /// Mint `amount` PT to `to`. Callable only by the configured minter.
    pub fn mint(env: Env, to: Address, amount: i128) {
        require_minter(&env);
        check_nonneg(&env, amount);
        let new_bal = read_balance(&env, &to)
            .checked_add(amount)
            .unwrap_or_else(|| panic_with_error!(&env, Error::MathOverflow));
        write_balance(&env, &to, new_bal);
        let new_supply = Self::total_supply(env.clone())
            .checked_add(amount)
            .unwrap_or_else(|| panic_with_error!(&env, Error::MathOverflow));
        env.storage()
            .instance()
            .set(&DataKey::TotalSupply, &new_supply);
        emit_transfer(&env, &env.current_contract_address(), &to, amount);
    }

    /// Burn `amount` PT from `from`. Callable only by the configured
    /// minter; bypasses allowance because the minter is the protocol.
    pub fn burn(env: Env, from: Address, amount: i128) {
        require_minter(&env);
        check_nonneg(&env, amount);
        let bal = read_balance(&env, &from);
        if bal < amount {
            panic_with_error!(&env, Error::InsufficientBalance);
        }
        write_balance(&env, &from, bal - amount);
        let supply = Self::total_supply(env.clone());
        env.storage()
            .instance()
            .set(&DataKey::TotalSupply, &(supply - amount));
        emit_transfer(&env, &from, &env.current_contract_address(), amount);
    }
}

// -------------------------------------------------------------------------
// Helpers
// -------------------------------------------------------------------------

fn require_minter(env: &Env) {
    let minter: Address = env
        .storage()
        .instance()
        .get(&DataKey::Minter)
        .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized));
    minter.require_auth();
}

fn check_nonneg(env: &Env, amount: i128) {
    if amount < 0 {
        panic_with_error!(env, Error::InsufficientBalance);
    }
}

fn read_balance(env: &Env, who: &Address) -> i128 {
    env.storage()
        .persistent()
        .get(&DataKey::Balance(who.clone()))
        .unwrap_or(0)
}

fn write_balance(env: &Env, who: &Address, bal: i128) {
    let key = DataKey::Balance(who.clone());
    env.storage().persistent().set(&key, &bal);
    env.storage()
        .persistent()
        .extend_ttl(&key, TTL_BUMP_THRESHOLD, EXTENDED_TTL);
}

fn do_transfer(env: &Env, from: &Address, to: &Address, amount: i128) {
    if amount == 0 {
        return;
    }
    let from_bal = read_balance(env, from);
    if from_bal < amount {
        panic_with_error!(env, Error::InsufficientBalance);
    }
    write_balance(env, from, from_bal - amount);
    let to_bal = read_balance(env, to)
        .checked_add(amount)
        .unwrap_or_else(|| panic_with_error!(env, Error::MathOverflow));
    write_balance(env, to, to_bal);
}

fn consume_allowance(env: &Env, from: &Address, spender: &Address, amount: i128) {
    let key = DataKey::Allowance(from.clone(), spender.clone());
    let current: i128 = env.storage().persistent().get(&key).unwrap_or(0);
    if current < amount {
        panic_with_error!(env, Error::Unauthorized);
    }
    env.storage().persistent().set(&key, &(current - amount));
}

fn emit_transfer(env: &Env, from: &Address, to: &Address, amount: i128) {
    env.events().publish(
        (symbol_short!("transfer"), from.clone(), to.clone()),
        amount,
    );
}
