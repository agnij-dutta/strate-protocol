//! Strate YT — Yield Token (SEP-41 + local yield-index accounting).
//!
//! Identical SEP-41 surface to PT, but YT also owns the yield-index
//! accounting that pays out variable yield. Every balance mutation
//! (mint / burn / transfer) settles the affected users locally — no
//! cross-contract callback into the minter — and the minter pushes
//! the new global index in on every sync.
//!
//! ## Re-entry rationale (H-01 in the Soroban host)
//!
//! Soroban 22.x forbids re-entering a contract already on the call
//! stack. The original architecture had YT call back into YS during
//! every balance change (to invoke `YS.settle_user`); that pattern
//! deadlocks on Soroban. Option B from `docs/handoff/H-01-reentry-fix.md`
//! moves the accrual state into YT itself, so the settlement is a
//! pure local read. YS pushes the new global index via
//! [`YtToken::push_yield_index`] and drains an account's accrued
//! pot via [`YtToken::drain_accrued`].
//!
//! ## State
//!
//! - `GlobalYieldIndex` (instance, WAD-scaled): the latest global
//!   yield-per-YT index, mirrored from YS via `push_yield_index`.
//! - `UserYieldIndex(addr)` (persistent, WAD-scaled): the snapshot
//!   of `GlobalYieldIndex` as of `addr`'s last settle.
//! - `AccruedYield(addr)` (persistent, underlying units): the amount
//!   `addr` has earned but not yet drained. `drain_accrued` zeroes
//!   this and returns it.
//!
//! ## Invariants
//!
//! - **M3 (Monotonicity of accrued yield)**: `push_yield_index`
//!   rejects any `new_index < current_index`. Equality is allowed.
//! - **U1 (NFL on YT transfer)**: settle runs on both sides before
//!   the balance moves, so a fresh recipient inherits the current
//!   global as their snapshot — no retroactive credit.
//! - **L1 (claim ≤ accrued)**: `drain_accrued` is atomic; the read
//!   of `AccruedYield` and the zero-write happen in the same call.
//!
//! Storage and TTL handling mirrors PT; see [`crate`] header docs there.

#![no_std]
#![deny(unsafe_code)]

use soroban_sdk::{
    contract, contractimpl, contracttype, panic_with_error, symbol_short, Address, Env, String,
};

use strate_interfaces::Error;
use strate_math::{mul_wad_down, WAD};

const EXTENDED_TTL: u32 = 500_000;
const TTL_BUMP_THRESHOLD: u32 = 100_000;

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
    /// Latest global yield index (WAD-scaled). Pushed in from YS via
    /// `push_yield_index`. Defaults to `WAD` on construction.
    GlobalYieldIndex,
    Balance(Address),
    Allowance(Address, Address),
    /// Per-user snapshot of the global index as of the user's last
    /// settle. WAD-scaled. Unset == "never settled".
    UserYieldIndex(Address),
    /// Per-user accrued-but-undrained yield (underlying units).
    AccruedYield(Address),
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
        s.set(&DataKey::GlobalYieldIndex, &WAD);
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

    // ---- Yield-index views ----------------------------------------------

    /// Current global yield index (WAD-scaled). Source of truth for
    /// every settle. Updated only by `push_yield_index`.
    pub fn global_yield_index(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&DataKey::GlobalYieldIndex)
            .unwrap_or(WAD)
    }

    /// Per-user snapshot of the global index as of `user`'s last
    /// settle. Returns 0 if `user` has never had a balance change.
    pub fn user_yield_index(env: Env, user: Address) -> i128 {
        read_user_yield_index(&env, &user)
    }

    /// What `user` would receive if `drain_accrued(user)` ran right
    /// now, **without** mutating state. Combines the persisted
    /// `AccruedYield` bucket with the not-yet-settled delta.
    pub fn accrued_yield(env: Env, user: Address) -> i128 {
        let global = Self::global_yield_index(env.clone());
        let user_idx = read_user_yield_index(&env, &user);
        let stored = read_accrued(&env, &user);
        let bal = read_balance(&env, &user);
        if bal <= 0 || user_idx <= 0 || global <= user_idx {
            return stored;
        }
        let delta = global - user_idx;
        stored.saturating_add(mul_wad_down(bal, delta))
    }

    // ---- SEP-41 balance / transfer / allowance ---------------------------

    pub fn balance(env: Env, id: Address) -> i128 {
        read_balance(&env, &id)
    }

    pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
        from.require_auth();
        check_nonneg(&env, amount);
        // H-01 mitigation: settle both sides locally *before* the
        // balance mutation. Self-transfer still settles once.
        let self_addr = env.current_contract_address();
        if from != self_addr {
            settle_user_local(&env, &from);
        }
        if to != self_addr && to != from {
            settle_user_local(&env, &to);
        }
        do_transfer(&env, &from, &to, amount);
        emit_transfer(&env, &from, &to, amount);
    }

    pub fn transfer_from(env: Env, spender: Address, from: Address, to: Address, amount: i128) {
        spender.require_auth();
        check_nonneg(&env, amount);
        consume_allowance(&env, &from, &spender, amount);
        let self_addr = env.current_contract_address();
        if from != self_addr {
            settle_user_local(&env, &from);
        }
        if to != self_addr && to != from {
            settle_user_local(&env, &to);
        }
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
    /// `to`'s settle runs before the balance increments (when `bal == 0`,
    /// settle is a no-op for accrual but always writes the snapshot —
    /// the C-02 line). The caller (YS) is expected to have already
    /// pushed the current global index in via `push_yield_index`.
    pub fn mint(env: Env, to: Address, amount: i128) {
        require_minter(&env);
        check_nonneg(&env, amount);
        let self_addr = env.current_contract_address();
        if to != self_addr {
            settle_user_local(&env, &to);
        }
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

    /// Burn from a holder. `from`'s settle runs first, so any pending
    /// yield is credited to their accrued pot before their balance
    /// shrinks — burning a position never costs yield that has
    /// already accrued against it.
    pub fn burn(env: Env, from: Address, amount: i128) {
        require_minter(&env);
        check_nonneg(&env, amount);
        let self_addr = env.current_contract_address();
        if from != self_addr {
            settle_user_local(&env, &from);
        }
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

    // ---- Yield-index minter surface -------------------------------------

    /// Push a new global yield index (WAD-scaled) into YT.
    ///
    /// **Minter-only.** Enforces strict monotonicity: `new_index` must
    /// be `>= GlobalYieldIndex`. A regression is rejected with
    /// `Error::OracleRejected` because that is the kind of bug the
    /// rate oracle would surface upstream; downstream consumers
    /// already treat that variant as "abort the action".
    ///
    /// Delta-cap and oracle-staleness checks live in YS (which owns
    /// the rate source); YT trusts its minter to have validated.
    pub fn push_yield_index(env: Env, new_index: i128) {
        require_minter(&env);
        if new_index <= 0 {
            panic_with_error!(&env, Error::OracleRejected);
        }
        let current = Self::global_yield_index(env.clone());
        if new_index < current {
            panic_with_error!(&env, Error::OracleRejected);
        }
        env.storage()
            .instance()
            .set(&DataKey::GlobalYieldIndex, &new_index);
    }

    /// Settle `user`'s pending yield into the `AccruedYield` bucket,
    /// then zero the bucket and return the total.
    ///
    /// **Minter-only.** Called by YS during `claim_yield` after it has
    /// pushed the latest global index, so the drain accounts for every
    /// wei `user` is owed up to the call site.
    pub fn drain_accrued(env: Env, user: Address) -> i128 {
        require_minter(&env);
        settle_user_local(&env, &user);
        let stored = read_accrued(&env, &user);
        if stored == 0 {
            return 0;
        }
        let k = DataKey::AccruedYield(user.clone());
        env.storage().persistent().set(&k, &0_i128);
        env.storage()
            .persistent()
            .extend_ttl(&k, TTL_BUMP_THRESHOLD, EXTENDED_TTL);
        stored
    }
}

// -------------------------------------------------------------------------
// Local settle path — the H-01 mitigation under Soroban call semantics.
// -------------------------------------------------------------------------

/// Roll `user`'s pending yield into the `AccruedYield` bucket and snap
/// their user index to the current global index.
///
/// **CRITICAL:** the snapshot write at the end is unconditional — it
/// runs even when the user's balance is zero. Without it, a recipient
/// of an inbound transfer would inherit the default `0` snapshot and
/// the next index advance would credit them retroactive yield (the
/// H-01 bug).
fn settle_user_local(env: &Env, user: &Address) {
    let global = read_global_yield_index(env);
    let user_idx = read_user_yield_index(env, user);
    let bal = read_balance(env, user);

    if bal > 0 && user_idx > 0 && global > user_idx {
        let delta = global - user_idx;
        let accrued = mul_wad_down(bal, delta);
        if accrued > 0 {
            let k = DataKey::AccruedYield(user.clone());
            let cur: i128 = env.storage().persistent().get(&k).unwrap_or(0);
            let new_total = cur
                .checked_add(accrued)
                .unwrap_or_else(|| panic_with_error!(env, Error::MathOverflow));
            env.storage().persistent().set(&k, &new_total);
            env.storage()
                .persistent()
                .extend_ttl(&k, TTL_BUMP_THRESHOLD, EXTENDED_TTL);
        }
    }

    let k = DataKey::UserYieldIndex(user.clone());
    env.storage().persistent().set(&k, &global);
    env.storage()
        .persistent()
        .extend_ttl(&k, TTL_BUMP_THRESHOLD, EXTENDED_TTL);
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

fn read_global_yield_index(env: &Env) -> i128 {
    env.storage()
        .instance()
        .get(&DataKey::GlobalYieldIndex)
        .unwrap_or(WAD)
}

fn read_user_yield_index(env: &Env, user: &Address) -> i128 {
    env.storage()
        .persistent()
        .get(&DataKey::UserYieldIndex(user.clone()))
        .unwrap_or(0)
}

fn read_accrued(env: &Env, user: &Address) -> i128 {
    env.storage()
        .persistent()
        .get(&DataKey::AccruedYield(user.clone()))
        .unwrap_or(0)
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
