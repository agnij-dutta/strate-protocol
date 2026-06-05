//! Strate yield-stripping core contract.
//!
//! Holds underlying, mints PT + YT 1:1, fetches the Blend rate via the
//! Oracle, derives a global yield index from it, and pushes that index
//! into YT — which owns the per-user accrual bookkeeping.
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
//!   `sync_yield_index()` *before* invoking `YT.mint`. The new YT
//!   holder's snapshot, written inside YT's settle-local before its
//!   balance increments, is therefore the freshly-pushed global
//!   index, not a stale one.
//!
//! - **H-01 — YT-transfer recipient stale snapshot**: YT settles both
//!   `from` and `to` *locally*, in its own contract, *before* mutating
//!   balances. The minter (this contract) is not on the call stack
//!   during a YT transfer, so the Soroban host's re-entry guard never
//!   trips. See `docs/handoff/H-01-reentry-fix.md` for the migration
//!   from the original callback topology.
//!
//! ## Storage and TTL
//!
//! YS now stores only: the immutable `Config`, pause state, the
//! `InitialBlendRate` baseline, the `LastIndexUpdate` ledger
//! timestamp, the `TotalUnderlying` accumulator, and the
//! `DeadSharesMinted` latch. The global yield index and per-user
//! accruals live in YT.

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
use strate_math::{div_wad_down, mul_wad_up};

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

/// YT mint/burn + yield-index client. Note: YT's mint/burn settle the
/// affected account locally (no callback into YS), and YS pushes the
/// global index in via `push_yield_index` on every sync. See
/// `docs/handoff/H-01-reentry-fix.md`.
#[contractclient(name = "YtClient")]
pub trait YtMintBurnYield {
    fn mint(env: Env, to: Address, amount: i128);
    fn burn(env: Env, from: Address, amount: i128);
    fn total_supply(env: Env) -> i128;
    fn balance(env: Env, id: Address) -> i128;
    fn global_yield_index(env: Env) -> i128;
    fn push_yield_index(env: Env, new_index: i128);
    fn accrued_yield(env: Env, user: Address) -> i128;
    fn drain_accrued(env: Env, user: Address) -> i128;
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
/// Re-exported under the historical name `Config` from
/// [`strate_types::YsConfig`] so external callers (factory, testutils,
/// integration tests) keep their existing import paths.
pub use strate_types::YsConfig as Config;

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
    /// Ledger timestamp of the last sync.
    LastIndexUpdate,
    /// Latched once the first deposit fires the C-01 dead-shares guard.
    DeadSharesMinted,
    /// Total underlying ever deposited (gross). For invariant checks.
    TotalUnderlying,
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
    /// The global index lives in YT and defaults to `WAD` there. The
    /// `InitialBlendRate` slot is left unset; the **first**
    /// `sync_yield_index` call latches whatever rate the Oracle
    /// reports as the baseline. This makes the constructor
    /// independent of Oracle liveness at deploy time.
    pub fn __constructor(env: Env, cfg: Config) {
        let s = env.storage().instance();
        s.set(&DataKey::Config, &cfg);
        s.set(&DataKey::Paused, &false);
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

    /// Read the current global yield index, WAD-scaled. Delegates to
    /// YT, which owns the source-of-truth value. Does **not** trigger
    /// a sync — that's the role of `sync_yield_index`.
    pub fn current_index(env: Env) -> i128 {
        let cfg = Self::config(env.clone());
        YtClient::new(&env, &cfg.yt).global_yield_index()
    }

    /// Read what `user` would receive if they called `claim_yield`
    /// right now, **without** mutating state. Delegates to YT's
    /// `accrued_yield`, which combines the persisted bucket with the
    /// not-yet-settled delta against the cached global index.
    pub fn pending_yield(env: Env, user: Address) -> i128 {
        let cfg = Self::config(env.clone());
        YtClient::new(&env, &cfg.yt).accrued_yield(&user)
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
    /// 2. `sync_yield_index` — pushes the fresh global index into YT
    ///    (C-02 fix: the snapshot YT writes for `to` is the post-sync
    ///    value).
    /// 3. C-01 dead-shares guard on the very first deposit.
    /// 4. Read state.
    /// 5. Effects (TotalUnderlying).
    /// 6. Interactions: pull underlying, call PT.mint, call YT.mint.
    ///    YT.mint settles `to` locally *before* incrementing balance.
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

        // C-02: refresh the global index in YT before any settle runs.
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
            // to the YS contract itself. See `docs/security/THREAT-MODEL.md` §C-01.
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
        let new_total = prev
            .checked_add(amount)
            .unwrap_or_else(|| panic_with_error!(&env, Error::MathOverflow));
        // tvl_cap = 0 means "no cap" — only valid post-audit; mainnet
        // markets always ship with cfg.tvl_cap > 0. The check applies
        // *before* persisting `TotalUnderlying`, and *before* any
        // underlying.transfer so a rejected mint touches nothing.
        if cfg.tvl_cap > 0 && new_total > cfg.tvl_cap {
            panic_with_error!(&env, Error::TvlCapExceeded);
        }
        env.storage()
            .instance()
            .set(&DataKey::TotalUnderlying, &new_total);

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
        // YT.mint settles `to` locally with the post-sync global
        // index, mitigating C-02 without re-entering YS.
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

        // Interactions: burn YT (settles `from` locally), burn PT, transfer
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
    ///
    /// **CEI order:** sync the index into YT, drain YT's accrued
    /// bucket for `user` (this also settles any pending delta inside
    /// YT), then transfer the underlying. Reversing the last two
    /// would open a TOCTOU window in which the index could advance
    /// between drain and transfer; do not do that.
    pub fn claim_yield(env: Env, user: Address) -> i128 {
        user.require_auth();
        // Pause does NOT block claim — exits remain open.
        let cfg = Self::config(env.clone());

        // Roll the global index forward before draining so `user`
        // collects every wei owed up to the current ledger.
        Self::sync_yield_index(env.clone());
        let user_idx_before = YtClient::new(&env, &cfg.yt).global_yield_index();
        let amount = YtClient::new(&env, &cfg.yt).drain_accrued(&user);
        let global = YtClient::new(&env, &cfg.yt).global_yield_index();

        if amount == 0 {
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

        // Interaction: pay out underlying.
        UnderlyingClient::new(&env, &cfg.underlying).transfer(
            &env.current_contract_address(),
            &user,
            &amount,
        );

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
    // Index sync
    // =====================================================================

    /// Refresh the global yield index from the Oracle and push it to YT.
    /// Permissionless.
    ///
    /// Steps:
    /// 1. Read current Blend rate via the Oracle client. On the **first**
    ///    sync, latch it as `InitialBlendRate` and exit (the index in
    ///    YT stays at `WAD` — there is no accrual to attribute
    ///    pre-launch).
    /// 2. Compute the candidate new global index:
    ///    `new = blend_rate / initial_rate` (in WAD).
    /// 3. Enforce monotonicity: `new >= old`. Oracle that goes backward
    ///    is rejected. (YT also enforces this on `push_yield_index`;
    ///    we keep both checks so YS surfaces the richer error context.)
    /// 4. Enforce delta cap: `(new - old) / old <= MAX_DELTA_PER_SYNC`.
    /// 5. Push to YT and emit `IndexSyncEvent`.
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

        let yt = YtClient::new(&env, &cfg.yt);
        let old_index = yt.global_yield_index();
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

        if new_index != old_index {
            yt.push_yield_index(&new_index);
        }
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
