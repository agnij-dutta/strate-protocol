//! Strate yield-stripping core contract.
//!
//! Holds underlying, mints PT + YT 1:1, tracks a global yield index
//! sourced from a rate oracle wrapping Blend, and pays out accrued
//! yield to YT holders on demand.
//!
//! ## Threat-model anchors
//!
//! - **C-01 — donation inflation**: on the first deposit
//!   (`total_underlying == 0`), the contract mints `DEAD_SHARES`
//!   (1 000 native units) of PT and YT to itself, locking those shares
//!   forever. A subsequent direct-transfer "donation" cannot inflate
//!   share price below the floor of `DEAD_SHARES`.
//!
//! - **C-02 — stale rate at YT mint**: `mint` calls
//!   `sync_yield_index()` *before* settling the recipient. The new YT
//!   holder's `UserIndex` is therefore the freshly-synced global index,
//!   not the cached one.
//!
//! - **H-01 — YT-transfer recipient stale snapshot**: the YT contract's
//!   `_update` hook calls [`Self::settle_user`] for **both** `from` and
//!   `to` *before* mutating balances. `settle_user` always writes the
//!   user index, even when balance is zero — this is the critical line
//!   the naive Pendle port misses.
//!
//! ## Storage and TTL
//!
//! Per ADR-004: Instance for the hot config; Persistent for balances /
//! per-user indices / supplies. State-mutating entry points extend
//! persistent TTL by `EXTENDED_TTL = 500_000` ledgers.

#![no_std]
#![deny(unsafe_code)]

use soroban_sdk::{
    contract, contractclient, contractimpl, contracttype, panic_with_error, symbol_short, Address,
    Env,
};

use strate_interfaces::events::{
    IndexSyncEvent, MintEvent, RedeemEvent, YieldClaimedEvent, TOPIC_INDEX, TOPIC_MINT,
    TOPIC_REDEEM, TOPIC_YIELD,
};
use strate_interfaces::Error;
use strate_math::{div_wad_down, mul_wad_down, mul_wad_up, WAD};

// =========================================================================
// Constants
// =========================================================================

/// Dead-shares minted on the very first deposit, locked at the YS
/// address forever. Sized to make the donation-inflation attack
/// (C-01) economically infeasible at any realistic donation amount.
/// Pendle uses 1 000; we follow that.
const DEAD_SHARES: i128 = 1_000;

/// Maximum relative increase of the global yield index permitted in
/// a single `sync_yield_index` call. WAD-scaled — 5e16 = 5%. Rejecting
/// implausible jumps freezes the Oracle path; the legitimate
/// recovery is for the admin to investigate.
const MAX_DELTA_PER_SYNC_WAD: i128 = 50_000_000_000_000_000; // 5%

/// Persistent TTL configuration (mirrors ADR-004).
const EXTENDED_TTL: u32 = 500_000;
const TTL_BUMP_THRESHOLD: u32 = 100_000;

/// Unpause timelock — 48h in seconds, per ADR-002 §"Emergency pause".
const UNPAUSE_TIMELOCK_SECS: u64 = 48 * 60 * 60;

// =========================================================================
// Cross-contract clients
// =========================================================================

/// Subset of the Oracle ABI consumed by YS. The Oracle contract owns
/// freshness / TWAP logic; YS only needs the latest rate.
#[contractclient(name = "OracleClient")]
pub trait RateOracle {
    /// Returns the current Blend `b_rate` in WAD scaling. Implementations
    /// are responsible for staleness and sanity checks.
    fn get_rate(env: Env) -> i128;
}

/// PT mint/burn client.
#[contractclient(name = "PtClient")]
pub trait PtMintBurn {
    fn mint(env: Env, to: Address, amount: i128);
    fn burn(env: Env, from: Address, amount: i128);
    fn total_supply(env: Env) -> i128;
    fn balance(env: Env, id: Address) -> i128;
}

/// YT mint/burn + balance client. Note: YT's `mint`/`burn` are the
/// versions defined in `yt-token` that invoke the `_update` hook
/// reentrantly back into `YieldStripping::settle_user` before any
/// balance change.
#[contractclient(name = "YtClient")]
pub trait YtMintBurn {
    fn mint(env: Env, to: Address, amount: i128);
    fn burn(env: Env, from: Address, amount: i128);
    fn total_supply(env: Env) -> i128;
    fn balance(env: Env, id: Address) -> i128;
}

/// Minimal SEP-41 underlying client.
#[contractclient(name = "UnderlyingClient")]
pub trait Underlying {
    fn transfer(env: Env, from: Address, to: Address, amount: i128);
    fn balance(env: Env, id: Address) -> i128;
}

// =========================================================================
// Storage
// =========================================================================

/// Immutable config — set once in `__constructor`, no setter.
#[contracttype]
#[derive(Clone)]
pub struct Config {
    pub underlying: Address,
    pub pt: Address,
    pub yt: Address,
    pub oracle: Address,
    pub maturity: u64,
    /// AMM curve parameter, immutable per market. YS doesn't use this
    /// directly; recorded for discoverability so external callers can
    /// read the market's curve scaling from a single contract.
    pub scalar_root: i128,
    /// Admin address — single role: pause/unpause.
    pub admin: Address,
}

#[contracttype]
#[derive(Clone)]
enum DataKey {
    // ---- Instance (hot) -----------------------------------------
    Config,
    /// `true` while paused. Pause is instant; unpause is timelocked.
    Paused,
    /// Pending unpause execution time (`now + 48h`); only set while a
    /// proposal is open.
    UnpauseEta,
    /// Initial Blend rate captured on the first `sync_yield_index`.
    /// Used as the denominator when computing the global index from
    /// any later Blend rate.
    InitialBlendRate,
    /// Global yield index, WAD-scaled.
    GlobalYieldIndex,
    /// Ledger timestamp of the last sync.
    LastIndexUpdate,
    /// Latched once the first deposit fires the C-01 dead-shares guard.
    DeadSharesMinted,
    /// Total underlying ever deposited (gross). For invariant checks.
    TotalUnderlying,

    // ---- Persistent (user-level) --------------------------------
    /// Per-user index snapshot, WAD-scaled.
    UserIndex(Address),
    /// Per-user claimable yield (in underlying units).
    AccruedYield(Address),
}

// =========================================================================
// Contract
// =========================================================================

#[contract]
pub struct YieldStripping;

#[contractimpl]
impl YieldStripping {
    /// Construct.
    ///
    /// Initialises the global index to `WAD` (= 1.0). The `InitialBlendRate`
    /// slot is left unset; the **first** `sync_yield_index` call latches
    /// whatever rate the Oracle reports as the baseline. This makes the
    /// constructor independent of Oracle liveness at deploy time.
    pub fn __constructor(env: Env, cfg: Config) {
        let s = env.storage().instance();
        s.set(&DataKey::Config, &cfg);
        s.set(&DataKey::Paused, &false);
        s.set(&DataKey::GlobalYieldIndex, &WAD);
        s.set(&DataKey::LastIndexUpdate, &env.ledger().timestamp());
        s.set(&DataKey::DeadSharesMinted, &false);
        s.set(&DataKey::TotalUnderlying, &0_i128);
    }

    // =====================================================================
    // Read-only views
    // =====================================================================

    pub fn config(env: Env) -> Config {
        env.storage()
            .instance()
            .get(&DataKey::Config)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized))
    }

    /// Read the current global yield index, WAD-scaled. Does **not**
    /// trigger a sync — that's the role of `sync_yield_index`.
    pub fn current_index(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&DataKey::GlobalYieldIndex)
            .unwrap_or(WAD)
    }

    /// Read what `user` would receive if they called `claim_yield` right
    /// now, **without** mutating state. The accrued amount is computed
    /// against the cached global index (no Oracle round-trip), so it
    /// reflects the index as last synced.
    pub fn pending_yield(env: Env, user: Address) -> i128 {
        let global = Self::current_index(env.clone());
        let user_idx = read_user_index(&env, &user);
        let stored: i128 = env
            .storage()
            .persistent()
            .get(&DataKey::AccruedYield(user.clone()))
            .unwrap_or(0);
        if user_idx == 0 {
            // Never settled; nothing pending against the index.
            return stored;
        }
        let yt = yt_balance(&env, &user);
        if yt == 0 || global <= user_idx {
            return stored;
        }
        let delta = global - user_idx;
        stored.saturating_add(mul_wad_down(yt, delta))
    }

    /// Whether the contract is paused.
    pub fn is_paused(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false)
    }

    /// Pending unpause ETA in seconds, or 0 if no proposal is open.
    pub fn unpause_eta(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&DataKey::UnpauseEta)
            .unwrap_or(0)
    }

    // =====================================================================
    // Mint / redeem / claim
    // =====================================================================

    /// Deposit underlying and mint matching PT+YT to `to`.
    ///
    /// Returns `(pt_minted, yt_minted, index_at_mint)`.
    ///
    /// **CEI order:**
    /// 1. `from.require_auth`, pause check, maturity check.
    /// 2. `sync_yield_index` (rolls the global index forward — C-02 fix).
    /// 3. C-01 dead-shares guard on the very first deposit.
    /// 4. Read state.
    /// 5. Effects (TotalUnderlying, user accruals).
    /// 6. Interactions: pull underlying, call PT.mint, call YT.mint.
    ///    YT.mint re-enters via `settle_user(to)` *before* incrementing
    ///    `to`'s balance — that settle picks up the **fresh** index.
    /// 7. Emit `MintEvent`.
    pub fn mint(env: Env, from: Address, to: Address, amount: i128) -> (i128, i128, i128) {
        from.require_auth();
        require_not_paused(&env);
        let cfg = Self::config(env.clone());
        if env.ledger().timestamp() >= cfg.maturity {
            panic_with_error!(&env, Error::MaturityReached);
        }
        if amount <= 0 {
            panic_with_error!(&env, Error::MinDepositNotMet);
        }

        // C-02: index must be fresh before the new YT holder's snapshot
        // is taken. The YT _update hook (invoked further down inside
        // YtClient::mint) reads `Self::settle_user`, which reads the
        // global index. Sync first to guarantee that read is fresh.
        Self::sync_yield_index(env.clone());

        // C-01: first-depositor dead-shares lock.
        let dead_done: bool = env
            .storage()
            .instance()
            .get(&DataKey::DeadSharesMinted)
            .unwrap_or(false);
        let mut pt_to_mint = amount;
        let mut yt_to_mint = amount;
        let mut dead_pt = 0_i128;
        let mut dead_yt = 0_i128;
        if !dead_done {
            // Reserve DEAD_SHARES from the user's mint and direct them
            // to the YS contract itself. The user *still* receives
            // `amount` PT/YT for `amount` underlying; the dead shares
            // are paid for by the protocol (the YS holds them
            // permanently and the underlying that backs them is locked
            // alongside). This trades a one-time tiny protocol cost for
            // permanent C-01 immunity. Documented rationale:
            // `docs/security/THREAT-MODEL.md` §C-01.
            if amount <= DEAD_SHARES {
                panic_with_error!(&env, Error::MinDepositNotMet);
            }
            dead_pt = DEAD_SHARES;
            dead_yt = DEAD_SHARES;
            pt_to_mint = amount - DEAD_SHARES;
            yt_to_mint = amount - DEAD_SHARES;
            env.storage()
                .instance()
                .set(&DataKey::DeadSharesMinted, &true);
        }

        // Effects: bookkeeping.
        let prev: i128 = env
            .storage()
            .instance()
            .get(&DataKey::TotalUnderlying)
            .unwrap_or(0);
        env.storage().instance().set(
            &DataKey::TotalUnderlying,
            &prev
                .checked_add(amount)
                .unwrap_or_else(|| panic_with_error!(&env, Error::MathOverflow)),
        );

        // Interactions.
        UnderlyingClient::new(&env, &cfg.underlying).transfer(
            &from,
            &env.current_contract_address(),
            &amount,
        );

        if dead_pt > 0 {
            let self_addr = env.current_contract_address();
            PtClient::new(&env, &cfg.pt).mint(&self_addr, &dead_pt);
            YtClient::new(&env, &cfg.yt).mint(&self_addr, &dead_yt);
        }
        PtClient::new(&env, &cfg.pt).mint(&to, &pt_to_mint);
        // YT.mint invokes settle_user(to) inside its _update hook
        // BEFORE incrementing `to`'s balance — the snapshot it writes
        // is the post-sync global index, mitigating C-02.
        YtClient::new(&env, &cfg.yt).mint(&to, &yt_to_mint);

        let index_at_mint = Self::current_index(env.clone());

        env.events().publish(
            (symbol_short!("mint"), to.clone()),
            MintEvent {
                user: to.clone(),
                underlying_in: amount,
                pt_out: pt_to_mint,
                yt_out: yt_to_mint,
                index_at_mint,
            },
        );
        let _ = TOPIC_MINT; // keep the topic constant linked.

        (pt_to_mint, yt_to_mint, index_at_mint)
    }

    /// Pre-maturity: burn equal PT and YT, return underlying 1:1.
    pub fn redeem_pair(env: Env, from: Address, amount: i128) -> i128 {
        from.require_auth();
        // Pause does NOT block redeem — ADR-002 §"While paused".
        let cfg = Self::config(env.clone());
        if amount <= 0 {
            panic_with_error!(&env, Error::InsufficientBalance);
        }
        Self::sync_yield_index(env.clone());

        // Effects.
        let prev: i128 = env
            .storage()
            .instance()
            .get(&DataKey::TotalUnderlying)
            .unwrap_or(0);
        if prev < amount {
            panic_with_error!(&env, Error::InsufficientBalance);
        }
        env.storage()
            .instance()
            .set(&DataKey::TotalUnderlying, &(prev - amount));

        // Interactions: burn YT (which settles `from`), burn PT, transfer
        // underlying.
        YtClient::new(&env, &cfg.yt).burn(&from, &amount);
        PtClient::new(&env, &cfg.pt).burn(&from, &amount);
        UnderlyingClient::new(&env, &cfg.underlying).transfer(
            &env.current_contract_address(),
            &from,
            &amount,
        );

        env.events().publish(
            (symbol_short!("redeem"), from.clone()),
            RedeemEvent {
                user: from.clone(),
                pt_burned: amount,
                yt_burned: amount,
                underlying_out: amount,
                redeem_kind: 0,
            },
        );
        let _ = TOPIC_REDEEM;

        amount
    }

    /// Post-maturity: burn PT alone, return underlying.
    pub fn redeem_pt(env: Env, from: Address, amount: i128) -> i128 {
        from.require_auth();
        // Pause does NOT block redeem.
        let cfg = Self::config(env.clone());
        if env.ledger().timestamp() < cfg.maturity {
            panic_with_error!(&env, Error::MaturityNotReached);
        }
        if amount <= 0 {
            panic_with_error!(&env, Error::InsufficientBalance);
        }
        // No sync needed: post-maturity yield index is frozen.

        let prev: i128 = env
            .storage()
            .instance()
            .get(&DataKey::TotalUnderlying)
            .unwrap_or(0);
        if prev < amount {
            panic_with_error!(&env, Error::InsufficientBalance);
        }
        env.storage()
            .instance()
            .set(&DataKey::TotalUnderlying, &(prev - amount));

        PtClient::new(&env, &cfg.pt).burn(&from, &amount);
        UnderlyingClient::new(&env, &cfg.underlying).transfer(
            &env.current_contract_address(),
            &from,
            &amount,
        );

        env.events().publish(
            (symbol_short!("redeem"), from.clone()),
            RedeemEvent {
                user: from.clone(),
                pt_burned: amount,
                yt_burned: 0,
                underlying_out: amount,
                redeem_kind: 1,
            },
        );

        amount
    }

    /// Claim accrued yield for `user`.
    pub fn claim_yield(env: Env, user: Address) -> i128 {
        user.require_auth();
        // Pause does NOT block claim — exits remain open.
        let cfg = Self::config(env.clone());

        // Sync + settle, in that order. After this call:
        //   AccruedYield[user] holds every wei `user` is owed up to now.
        //   UserIndex[user] == GlobalYieldIndex.
        Self::sync_yield_index(env.clone());
        let user_idx_before = read_user_index(&env, &user);
        Self::settle_user(env.clone(), user.clone());

        let amount: i128 = env
            .storage()
            .persistent()
            .get(&DataKey::AccruedYield(user.clone()))
            .unwrap_or(0);
        if amount == 0 {
            let global = Self::current_index(env.clone());
            env.events().publish(
                (symbol_short!("yield"), user.clone()),
                YieldClaimedEvent {
                    user: user.clone(),
                    yield_amount: 0,
                    user_index_before: user_idx_before,
                    user_index_after: global,
                },
            );
            let _ = TOPIC_YIELD;
            return 0;
        }

        // Effects: zero the claimable.
        env.storage()
            .persistent()
            .set(&DataKey::AccruedYield(user.clone()), &0_i128);

        // Interaction: pay out underlying.
        UnderlyingClient::new(&env, &cfg.underlying).transfer(
            &env.current_contract_address(),
            &user,
            &amount,
        );

        let global = Self::current_index(env.clone());
        env.events().publish(
            (symbol_short!("yield"), user.clone()),
            YieldClaimedEvent {
                user: user.clone(),
                yield_amount: amount,
                user_index_before: user_idx_before,
                user_index_after: global,
            },
        );

        amount
    }

    // =====================================================================
    // Index sync + settle
    // =====================================================================

    /// Refresh the global yield index from the Oracle. Permissionless.
    ///
    /// Steps:
    /// 1. Read current Blend rate via the Oracle client. On the **first**
    ///    sync, latch it as `InitialBlendRate` and exit (the index stays
    ///    at `WAD` — there is no accrual to attribute pre-launch).
    /// 2. Compute the candidate new global index:
    ///    `new = blend_rate / initial_rate` (in WAD).
    /// 3. Enforce monotonicity: `new >= old`. Oracle that goes backward
    ///    is rejected.
    /// 4. Enforce delta cap: `(new - old) / old <= MAX_DELTA_PER_SYNC`.
    /// 5. Commit and emit `IndexSyncEvent`.
    pub fn sync_yield_index(env: Env) {
        let cfg = Self::config(env.clone());
        // Post-maturity: index is frozen.
        if env.ledger().timestamp() >= cfg.maturity {
            return;
        }
        let oracle = OracleClient::new(&env, &cfg.oracle);
        let blend_rate = oracle.get_rate();
        if blend_rate <= 0 {
            panic_with_error!(&env, Error::OracleRejected);
        }
        let s = env.storage().instance();

        let initial: Option<i128> = s.get(&DataKey::InitialBlendRate);
        let initial = match initial {
            Some(v) => v,
            None => {
                s.set(&DataKey::InitialBlendRate, &blend_rate);
                s.set(&DataKey::LastIndexUpdate, &env.ledger().timestamp());
                return;
            }
        };

        let old_index: i128 = s.get(&DataKey::GlobalYieldIndex).unwrap_or(WAD);
        // new = blend_rate / initial * WAD
        let new_index = div_wad_down(blend_rate, initial);

        if new_index < old_index {
            // Strict monotonicity. Reject. Note: equality is fine.
            panic_with_error!(&env, Error::OracleRejected);
        }
        if new_index > old_index {
            let delta = new_index - old_index;
            // delta / old_index <= MAX_DELTA_PER_SYNC_WAD
            // i.e. delta * WAD <= MAX_DELTA_PER_SYNC_WAD * old_index
            //   == mul_wad_up(old_index, MAX_DELTA_PER_SYNC_WAD)
            let bound = mul_wad_up(old_index, MAX_DELTA_PER_SYNC_WAD);
            if delta > bound {
                panic_with_error!(&env, Error::OracleRejected);
            }
        }

        s.set(&DataKey::GlobalYieldIndex, &new_index);
        s.set(&DataKey::LastIndexUpdate, &env.ledger().timestamp());

        env.events().publish(
            (symbol_short!("indexsync"),),
            IndexSyncEvent {
                new_index,
                old_index,
                at_timestamp: env.ledger().timestamp(),
            },
        );
        let _ = TOPIC_INDEX;
    }

    /// Settle `user`'s yield up to the current global index.
    ///
    /// **CRITICAL — H-01 mitigation.** The YT contract's `_update`
    /// hook invokes this *before* any balance mutation. The two
    /// non-obvious lines are:
    ///
    /// 1. `sync_yield_index` is called first, so the snapshot we write
    ///    is the post-sync global value.
    ///
    /// 2. `UserIndex[user]` is updated **even when YT balance is zero**.
    ///    Without this, a recipient who just received an inbound
    ///    transfer (balance still 0 at the moment of settle, balance > 0
    ///    immediately after) would inherit the *old* snapshot and the
    ///    next index advance would credit them yield they didn't earn.
    ///    This is the line every Pendle port misses.
    ///
    /// Re-entrant via the YT `_update` hook: settle_user → YT.balance()
    /// is a read-only call on a separate contract instance; safe.
    pub fn settle_user(env: Env, user: Address) {
        // No `require_auth` here: settle is idempotent and self-bookkeeping.
        // It is callable by the YT contract via the _update hook, and
        // also by entry-point flows like `claim_yield` that hold caller
        // auth on their own.
        Self::sync_yield_index(env.clone());

        let global = Self::current_index(env.clone());
        let user_idx = read_user_index(&env, &user);
        let yt_bal = yt_balance(&env, &user);

        if yt_bal > 0 && user_idx > 0 && global > user_idx {
            let delta = global - user_idx;
            let accrued = mul_wad_down(yt_bal, delta);
            let current: i128 = env
                .storage()
                .persistent()
                .get(&DataKey::AccruedYield(user.clone()))
                .unwrap_or(0);
            let new_total = current
                .checked_add(accrued)
                .unwrap_or_else(|| panic_with_error!(&env, Error::MathOverflow));
            let k = DataKey::AccruedYield(user.clone());
            env.storage().persistent().set(&k, &new_total);
            env.storage()
                .persistent()
                .extend_ttl(&k, TTL_BUMP_THRESHOLD, EXTENDED_TTL);
        }

        // ALWAYS write the snapshot. This is the H-01 line.
        let k = DataKey::UserIndex(user.clone());
        env.storage().persistent().set(&k, &global);
        env.storage()
            .persistent()
            .extend_ttl(&k, TTL_BUMP_THRESHOLD, EXTENDED_TTL);
    }

    // =====================================================================
    // Admin: pause / unpause (asymmetric per ADR-002)
    // =====================================================================

    /// Instantly pause. Blocks `mint`; never blocks `claim`/`redeem`.
    pub fn pause(env: Env) {
        Self::config(env.clone()).admin.require_auth();
        env.storage().instance().set(&DataKey::Paused, &true);
        // Cancel any pending unpause proposal.
        env.storage().instance().remove(&DataKey::UnpauseEta);
    }

    /// Open an unpause proposal. The actual unpause executes after
    /// `UNPAUSE_TIMELOCK_SECS`. Admin-only.
    pub fn propose_unpause(env: Env) {
        Self::config(env.clone()).admin.require_auth();
        let eta = env
            .ledger()
            .timestamp()
            .saturating_add(UNPAUSE_TIMELOCK_SECS);
        env.storage().instance().set(&DataKey::UnpauseEta, &eta);
    }

    /// Execute the unpause once `now >= eta`.
    pub fn execute_unpause(env: Env) {
        Self::config(env.clone()).admin.require_auth();
        let eta: u64 = env
            .storage()
            .instance()
            .get(&DataKey::UnpauseEta)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));
        if env.ledger().timestamp() < eta {
            panic_with_error!(&env, Error::Unauthorized);
        }
        env.storage().instance().set(&DataKey::Paused, &false);
        env.storage().instance().remove(&DataKey::UnpauseEta);
    }
}

// =========================================================================
// Internal helpers
// =========================================================================

fn require_not_paused(env: &Env) {
    let p: bool = env
        .storage()
        .instance()
        .get(&DataKey::Paused)
        .unwrap_or(false);
    if p {
        panic_with_error!(env, Error::Paused);
    }
}

fn read_user_index(env: &Env, user: &Address) -> i128 {
    env.storage()
        .persistent()
        .get(&DataKey::UserIndex(user.clone()))
        .unwrap_or(0)
}

fn yt_balance(env: &Env, user: &Address) -> i128 {
    let cfg: Config = match env.storage().instance().get(&DataKey::Config) {
        Some(c) => c,
        None => return 0,
    };
    YtClient::new(env, &cfg.yt).balance(user)
}
