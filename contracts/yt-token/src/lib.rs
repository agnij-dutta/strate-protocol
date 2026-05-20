//! Strate YT — Yield Token (SEP-41 + `_update` hook).
//!
//! Identical SEP-41 surface to PT, but every balance mutation routes
//! through an `_update` hook that cross-contract-calls
//! `YieldStripping::settle_user(addr)` for **both** `from` and `to`
//! *before* the balance mutates. This is the H-01 mitigation in
//! `docs/security/THREAT-MODEL.md` — the bug nearly every Pendle port
//! has shipped with at least once.
//!
//! Mint-from-zero and burn-to-zero are no-ops on the zero-address side
//! (we use `current_contract_address` as the sentinel for "no
//! counterparty"). See ADR-001 §3.
//!
//! Storage and TTL handling mirrors PT; see [`crate`] header docs there.

#![no_std]
#![deny(unsafe_code)]

use soroban_sdk::{
    contract, contractclient, contractimpl, contracttype, panic_with_error, symbol_short, Address,
    Env, String,
};

use strate_interfaces::Error;

const EXTENDED_TTL: u32 = 500_000;
const TTL_BUMP_THRESHOLD: u32 = 100_000;

// -------------------------------------------------------------------------
// Cross-contract client into YieldStripping (used by the _update hook).
//
// We don't pull `yield-stripping` as a dependency to avoid a dependency
// cycle (YS depends on YT for the YT mint cross-call in the other
// direction). Instead we hand-declare the one method we need against a
// stable signature.
// -------------------------------------------------------------------------

/// Subset of `IYieldStripping` exposed for the `_update` hook.
#[contractclient(name = "YsHookClient")]
pub trait YsHook {
    /// Settle `user`'s accrued yield into their claimable pot and snap
    /// their user index to the current global index. Idempotent within
    /// one ledger.
    fn settle_user(env: Env, user: Address);
}

// -------------------------------------------------------------------------
// Storage layout
// -------------------------------------------------------------------------

#[contracttype]
#[derive(Clone)]
enum DataKey {
    Minter,
    Underlying,
    Maturity,
    Name,
    Symbol,
    Decimals,
    TotalSupply,
    Balance(Address),
    Allowance(Address, Address),
}

/// Constructor input. Mirror of `PtInitArgs`.
#[contracttype]
#[derive(Clone)]
pub struct YtInitArgs {
    pub underlying: Address,
    pub maturity: u64,
    pub name: String,
    pub symbol: String,
    pub decimals: u32,
}

// -------------------------------------------------------------------------
// Contract
// -------------------------------------------------------------------------

#[contract]
pub struct YtToken;

#[contractimpl]
impl YtToken {
    pub fn __constructor(env: Env, args: YtInitArgs) {
        let s = env.storage().instance();
        s.set(&DataKey::Underlying, &args.underlying);
        s.set(&DataKey::Maturity, &args.maturity);
        s.set(&DataKey::Name, &args.name);
        s.set(&DataKey::Symbol, &args.symbol);
        s.set(&DataKey::Decimals, &args.decimals);
        s.set(&DataKey::TotalSupply, &0_i128);
    }

    /// One-shot minter bootstrap. See `pt_token::PtToken::initialize_minter`.
    pub fn initialize_minter(env: Env, minter: Address) {
        let s = env.storage().instance();
        if s.has(&DataKey::Minter) {
            panic_with_error!(&env, Error::AlreadyInitialized);
        }
        s.set(&DataKey::Minter, &minter);
    }

    // ---- SEP-41 metadata -------------------------------------------------

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

    pub fn maturity(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&DataKey::Maturity)
            .unwrap_or(0)
    }

    pub fn minter(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::Minter)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized))
    }

    pub fn total_supply(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&DataKey::TotalSupply)
            .unwrap_or(0)
    }

    // ---- SEP-41 balance / transfer / allowance ---------------------------

    pub fn balance(env: Env, id: Address) -> i128 {
        read_balance(&env, &id)
    }

    pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
        from.require_auth();
        check_nonneg(&env, amount);
        // Two settle_user calls — H-01 mitigation. Both run *before* the
        // balance mutation. Self-transfer is a no-op (still settles).
        invoke_update_hook(&env, Some(&from), Some(&to));
        do_transfer(&env, &from, &to, amount);
        emit_transfer(&env, &from, &to, amount);
    }

    pub fn transfer_from(env: Env, spender: Address, from: Address, to: Address, amount: i128) {
        spender.require_auth();
        check_nonneg(&env, amount);
        consume_allowance(&env, &from, &spender, amount);
        invoke_update_hook(&env, Some(&from), Some(&to));
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

    // ---- Privileged mint / burn -----------------------------------------

    /// Mint with a fresh index snapshot.
    ///
    /// Behaviour: invokes the `_update` hook with `from = None` (mint
    /// from zero — no-op on the zero side) and `to = Some(to)` so the
    /// recipient's index snapshot is set to the **current** global
    /// index *before* the balance increments. This is the C-02
    /// mitigation: a brand-new YT holder cannot retroactively claim
    /// any pre-mint yield.
    pub fn mint(env: Env, to: Address, amount: i128) {
        require_minter(&env);
        check_nonneg(&env, amount);
        invoke_update_hook(&env, None, Some(&to));
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

    /// Burn from a holder. Hook is invoked with `to = None` (burn to
    /// zero). The from-side settle credits any pending yield to the
    /// holder's claimable balance *before* their YT balance shrinks —
    /// so burning a position never costs yield that has already accrued
    /// against it.
    pub fn burn(env: Env, from: Address, amount: i128) {
        require_minter(&env);
        check_nonneg(&env, amount);
        invoke_update_hook(&env, Some(&from), None);
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
// _update hook
// -------------------------------------------------------------------------

/// Cross-contract call: invoke `settle_user(addr)` on the minter for
/// every non-zero side of a balance change.
///
/// CRITICAL: this is the H-01 mitigation. Both `from` and `to` are
/// settled here, *before* the balance mutation runs. The zero side
/// (mint-from-zero, burn-to-zero) is skipped.
fn invoke_update_hook(env: &Env, from: Option<&Address>, to: Option<&Address>) {
    let minter_addr: Address = match env.storage().instance().get::<_, Address>(&DataKey::Minter) {
        Some(m) => m,
        // Pre-init no-op so test fixtures can preload balances. In
        // production the YT is always initialised before the first
        // `mint`.
        None => return,
    };
    let client = YsHookClient::new(env, &minter_addr);
    let self_addr = env.current_contract_address();
    if let Some(f) = from {
        if f != &self_addr {
            client.settle_user(f);
        }
    }
    if let Some(t) = to {
        if t != &self_addr {
            client.settle_user(t);
        }
    }
}

// -------------------------------------------------------------------------
// Internal helpers (mirror of PT)
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
