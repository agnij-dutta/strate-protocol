//! Strate Oracle.
//!
//! TWAP wrapper over Blend's `b_rate` exchange rate, with rate-limit and
//! freeze-on-violation guards. Consumed by YieldStripping (for the cached
//! exchange rate driving the yield index) and by the AMM (for TWAP
//! observations of the implied rate).
//!
//! ## Responsibilities
//!
//! 1. **Cache Blend's exchange rate.** `sync_rate()` cross-contract-calls
//!    Blend's `get_reserve_data().b_rate` and stores it after passing
//!    monotonicity + per-sync delta limit checks. The cache, not the
//!    spot read, is what consumers see.
//! 2. **TWAP observations.** A ring buffer of `(price, ts)` samples. The
//!    AMM pushes one per swap via `update_observation()`; `get_twap()`
//!    returns the time-weighted average over the buffer.
//! 3. **Freeze on violation.** A rate that decreases (monotonicity break)
//!    or jumps more than `max_delta_per_sync_wad` triggers a freeze: the
//!    oracle stops serving fresh reads and waits for the admin to
//!    `reattest` via a 48h timelock.
//!
//! ## Rate scaling
//!
//! Blend stores `b_rate` in 1e9 fixed point. We re-scale to WAD (1e18)
//! at the read boundary so the rest of the math (and consumers) stays in
//! WAD. All `i128` rate values returned by `get_rate`, `get_twap`, and
//! the cached storage are WAD-scaled.
//!
//! ## ADR references
//!
//! - ADR-001 (call protocol) — Oracle is read-only with respect to its
//!   peers; the only outbound call is into Blend (or `MockBlendPool` in
//!   tests). YS and AMM call inward.
//! - ADR-002 (admin surface) — `blend_pool` is immutable; admin can
//!   pause/unpause/reattest only.
//! - ADR-004 (storage) — `Config`, `Paused`, `Frozen` in instance.
//!   Observations and the cached rate live in persistent storage so
//!   long-dated markets survive without touching the oracle.
//! - ADR-005 (upgrades) — reattest is a 48h timelocked operation; the
//!   oracle itself is not upgradable.

#![no_std]
#![deny(unsafe_code)]
#![deny(clippy::unwrap_used)]
#![warn(clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::needless_pass_by_value)]
#![allow(clippy::similar_names)]

use soroban_sdk::{
    contract, contractclient, contractimpl, contracttype, panic_with_error, symbol_short, Address,
    Env, Symbol,
};

use strate_interfaces::events::{IndexSyncEvent, ObservationEvent, TOPIC_INDEX, TOPIC_OBS};
use strate_interfaces::Error;
use strate_math::{div_wad_checked, WAD};

// -------------------------------------------------------------------------
// Constants.
// -------------------------------------------------------------------------

/// Seconds per ledger on Stellar (target ~5s).
pub const SECONDS_PER_LEDGER: u64 = 5;

/// Default 48h timelock for `reattest`, in seconds. Matches ADR-005.
pub const REATTEST_TIMELOCK_SECONDS: u64 = 48 * 60 * 60;

/// Persistent TTL bump policy (ADR-004 defaults).
pub const TTL_BUMP_THRESHOLD: u32 = 100_000;
/// Target TTL when we extend.
pub const EXTENDED_TTL: u32 = 500_000;

/// Topic tag for the `OracleFrozen` event.
pub const TOPIC_FROZEN: Symbol = symbol_short!("freeze");
/// Topic tag for the `OracleReattested` event.
pub const TOPIC_REATTEST: Symbol = symbol_short!("reattest");
/// Topic tag for pause / unpause events.
pub const TOPIC_PAUSE: Symbol = symbol_short!("pause");

/// Scaling factor from Blend's 1e9 fixed-point to WAD (1e18).
const BLEND_TO_WAD_SCALE: i128 = 1_000_000_000;

// -------------------------------------------------------------------------
// Storage types.
// -------------------------------------------------------------------------

/// Immutable + admin-tunable oracle parameters.
///
/// `blend_pool` is set once at construction and never mutated (ADR-002).
/// The remaining fields are intentionally part of `Config` so a single
/// `instance.get` returns the entire config in one shot; this is the
/// hottest read path so we trade a bit of write-cost for read efficiency.
#[contracttype]
#[derive(Clone, Debug)]
pub struct OracleConfig {
    /// Address of the Blend lending pool we read `b_rate` from.
    pub blend_pool: Address,
    /// Maximum allowed relative jump per `sync_rate`, in WAD.
    /// `5e16` == 5%. A rate moving more than this freezes the oracle.
    pub max_delta_per_sync_wad: i128,
    /// Maximum age (in ledger units) of `CachedRate` before reads revert.
    /// At 5s/ledger, 120 ledgers ≈ 10 minutes.
    pub max_staleness_ledgers: u32,
    /// Size of the TWAP ring buffer. Bounded so it always fits in
    /// instance budget. Values < 2 are rejected at construction.
    pub observation_window: u32,
}

/// One TWAP observation. `price` is WAD-scaled; `ts` is ledger
/// timestamp in seconds.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observation {
    /// WAD-scaled price (typically the AMM's implied rate).
    pub price: i128,
    /// Ledger timestamp of the sample, in seconds.
    pub ts: u64,
}

/// Pending reattest proposal, recorded when the admin first calls
/// `reattest(rate)` while the oracle is frozen. After `eta` the same
/// admin (or anyone — execution is permissionless) can finalize via
/// `execute_reattest`.
#[contracttype]
#[derive(Clone, Debug)]
pub struct PendingReattest {
    /// The rate the admin attests to (WAD-scaled).
    pub rate: i128,
    /// Earliest ledger timestamp at which `execute_reattest` may run.
    pub eta: u64,
}

/// Pending unpause proposal — pause is instant, unpause is 48h.
#[contracttype]
#[derive(Clone, Debug)]
pub struct PendingUnpause {
    /// Earliest ledger timestamp at which `execute_unpause` may run.
    pub eta: u64,
}

/// Storage layout discriminants.
#[contracttype]
#[derive(Clone, Debug)]
pub enum DataKey {
    /// Instance: hot config (read on every call).
    Config,
    /// Instance: admin address.
    Admin,
    /// Instance: paused flag.
    Paused,
    /// Instance: frozen flag.
    Frozen,
    /// Instance: pending unpause proposal.
    PendingUnpause,
    /// Instance: pending reattest proposal.
    PendingReattest,
    /// Persistent: last known good rate, WAD.
    CachedRate,
    /// Persistent: ledger timestamp at which `CachedRate` was written.
    CachedRateAt,
    /// Persistent: ring buffer slot (0..observation_window).
    Observations(u32),
    /// Persistent: head pointer into the ring buffer.
    ObservationHead,
    /// Persistent: how many observations have ever been written
    /// (capped at `observation_window`). Distinguishes "buffer not full"
    /// from "fully populated buffer with old data".
    ObservationCount,
}

// -------------------------------------------------------------------------
// Blend pool client (minimal — we only call `get_reserve_data`).
// -------------------------------------------------------------------------

/// Reserve-data shape exposed by Blend's pool. We mirror only the fields
/// Strate consumes; this struct's serde-layout must match
/// `MockBlendPool::MockReserveData` in `strate-testutils`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReserveData {
    /// `b_rate`, 1e9-scaled.
    pub b_rate: i128,
    /// Last accrual timestamp inside Blend.
    pub last_time: u64,
    /// Underlying reserve held by the pool (unused by the oracle).
    pub reserve: i128,
}

/// Generated client stub for the Blend pool. Soroban's
/// `contractclient` macro builds the cross-contract call shim from the
/// method signature.
#[contractclient(name = "BlendPoolClient")]
pub trait BlendPool {
    /// Fetch the pool's reserve data.
    fn get_reserve_data(env: Env) -> ReserveData;
}

// -------------------------------------------------------------------------
// Oracle contract.
// -------------------------------------------------------------------------

/// The Oracle contract type. Stateless aside from storage.
#[contract]
pub struct Oracle;

#[contractimpl]
impl Oracle {
    /// Initialise the oracle. Idempotent against re-init: a second call
    /// reverts with `AlreadyInitialized`.
    ///
    /// Validates:
    /// - `max_delta_per_sync_wad > 0 && < WAD` (a 100%+ allowed delta
    ///   would disable the rate-limit entirely).
    /// - `observation_window >= 2` (a 1-slot buffer cannot do TWAP).
    /// - `max_staleness_ledgers > 0`.
    ///
    /// # Constructor wiring
    ///
    /// Soroban's `__constructor` runs once on `register()`. We treat the
    /// `admin` arg as the multisig/timelocked controller (ADR-002).
    pub fn __constructor(env: Env, admin: Address, cfg: OracleConfig) {
        if instance_has(&env, &DataKey::Config) {
            panic_with_error!(&env, Error::AlreadyInitialized);
        }
        validate_config(&env, &cfg);

        env.storage().instance().set(&DataKey::Config, &cfg);
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Paused, &false);
        env.storage().instance().set(&DataKey::Frozen, &false);

        // Seed the ring buffer head/count and the cached-rate slot. The
        // very first `sync_rate()` will populate the cache from Blend.
        env.storage()
            .persistent()
            .set(&DataKey::ObservationHead, &0_u32);
        env.storage()
            .persistent()
            .set(&DataKey::ObservationCount, &0_u32);
        // Set CachedRate to a sentinel of 0; downstream code treats 0 as
        // "not yet synced" and skips the monotonicity check on first sync.
        env.storage().persistent().set(&DataKey::CachedRate, &0_i128);
        env.storage()
            .persistent()
            .set(&DataKey::CachedRateAt, &0_u64);

        bump_persistent(&env, &DataKey::CachedRate);
        bump_persistent(&env, &DataKey::CachedRateAt);
        bump_persistent(&env, &DataKey::ObservationHead);
        bump_persistent(&env, &DataKey::ObservationCount);
    }

    /// Refresh the cached rate from Blend.
    ///
    /// Permissionless: anyone can prod the oracle. Validates
    /// monotonicity and the per-sync delta cap; on violation the oracle
    /// freezes, the violation is emitted as an event, and the
    /// **last-known-good** cached rate is returned. The freeze flag
    /// must persist across the call, so this function deliberately
    /// returns `Ok(cached_rate)` instead of `Err`: a Soroban `Err`
    /// return rolls back storage writes, which would defeat the freeze.
    /// Consumers must check `is_frozen()` after `sync_rate` and refuse
    /// to proceed if it returns `true`.
    ///
    /// Returns:
    /// - `Ok(new_rate)` on a clean sync.
    /// - `Ok(cached_rate)` (the last-known-good rate) on a violation;
    ///   the oracle is frozen as a side effect.
    /// - `Err(Paused)` if the oracle is paused.
    /// - `Err(OracleRejected)` only if the oracle was already frozen
    ///   before the call, or Blend returned a non-positive rate.
    pub fn sync_rate(env: Env) -> Result<i128, Error> {
        check_not_paused(&env)?;
        check_not_frozen(&env)?;

        let cfg = read_config(&env);
        let new_rate = fetch_blend_rate(&env, &cfg.blend_pool)?;

        let cached_rate = read_cached_rate(&env);
        let now = env.ledger().timestamp();

        // On first sync (`cached_rate == 0`) we accept any positive
        // value Blend reports. The delta-cap kicks in from the second
        // sync onward.
        if cached_rate > 0 {
            // Monotonicity: rate cannot decrease.
            if new_rate < cached_rate {
                freeze(&env);
                emit_freeze(&env, FreezeReason::Monotonicity, new_rate, cached_rate);
                return Ok(cached_rate);
            }

            // Per-sync relative delta cap.
            let delta = new_rate - cached_rate;
            let delta_wad = div_wad_checked(delta, cached_rate)
                .map_err(|_| Error::OracleRejected)?;
            if delta_wad > cfg.max_delta_per_sync_wad {
                freeze(&env);
                emit_freeze(&env, FreezeReason::Delta, new_rate, cached_rate);
                return Ok(cached_rate);
            }
        }

        // Accepted — commit.
        write_cached_rate(&env, new_rate, now);
        emit_index_sync(&env, new_rate, cached_rate, now);
        Ok(new_rate)
    }

    /// Push an observation into the TWAP ring buffer.
    ///
    /// Called by the AMM on each swap. Auth is not enforced at the
    /// contract level (the AMM signs the call as itself, which is the
    /// only address that should bother); the worst an adversary can do
    /// by spamming is write their own price into the buffer, which the
    /// time-weighted formula naturally amortises — and the AMM's lock
    /// against same-block reads (ADR-001 §4) eliminates the single-block
    /// manipulation vector.
    pub fn update_observation(env: Env, price: i128) -> Result<(), Error> {
        check_not_paused(&env)?;
        check_not_frozen(&env)?;
        if price <= 0 {
            return Err(Error::OracleRejected);
        }
        let cfg = read_config(&env);
        let head: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::ObservationHead)
            .unwrap_or(0);
        let count: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::ObservationCount)
            .unwrap_or(0);

        let now = env.ledger().timestamp();
        let obs = Observation { price, ts: now };

        env.storage()
            .persistent()
            .set(&DataKey::Observations(head), &obs);

        let next_head = (head + 1) % cfg.observation_window;
        let new_count = if count < cfg.observation_window {
            count + 1
        } else {
            count
        };
        env.storage()
            .persistent()
            .set(&DataKey::ObservationHead, &next_head);
        env.storage()
            .persistent()
            .set(&DataKey::ObservationCount, &new_count);

        bump_persistent(&env, &DataKey::Observations(head));
        bump_persistent(&env, &DataKey::ObservationHead);
        bump_persistent(&env, &DataKey::ObservationCount);

        env.events()
            .publish((TOPIC_OBS,), ObservationEvent {
                implied_rate: price,
                at_timestamp: now,
                slot: head,
            });

        Ok(())
    }

    /// View: latest cached rate, WAD-scaled. Reverts with `OracleStale`
    /// if the cache hasn't been refreshed within the configured
    /// staleness window. Consumers should call `sync_rate` and retry.
    pub fn get_rate(env: Env) -> Result<i128, Error> {
        check_not_paused(&env)?;
        check_not_frozen(&env)?;
        let cached = read_cached_rate(&env);
        if cached == 0 {
            // Never synced.
            return Err(Error::OracleStale);
        }
        let cached_at = read_cached_at(&env);
        let now = env.ledger().timestamp();
        let cfg = read_config(&env);
        let max_staleness_secs = u64::from(cfg.max_staleness_ledgers) * SECONDS_PER_LEDGER;
        let staleness = now.saturating_sub(cached_at);
        if staleness > max_staleness_secs {
            return Err(Error::OracleStale);
        }
        Ok(cached)
    }

    /// View: TWAP across the populated portion of the ring buffer.
    ///
    /// Reverts with `OracleStale` if fewer than 2 observations are
    /// recorded (TWAP is undefined). The weight of each observation is
    /// `ts[i+1] - ts[i]`; the final observation runs to `now`.
    pub fn get_twap(env: Env) -> Result<i128, Error> {
        check_not_paused(&env)?;
        check_not_frozen(&env)?;
        let cfg = read_config(&env);
        let count: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::ObservationCount)
            .unwrap_or(0);
        if count < 2 {
            return Err(Error::OracleStale);
        }
        let head: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::ObservationHead)
            .unwrap_or(0);

        // Walk oldest -> newest. With a full buffer of size N and head
        // pointing at the slot to be overwritten next, the oldest entry
        // is at `head` (it's the one that hasn't been clobbered since
        // the last wrap). If `count < N`, oldest is at slot 0.
        let n = cfg.observation_window;
        let (start, len) = if count < n {
            (0_u32, count)
        } else {
            (head, n)
        };

        compute_twap(&env, start, len, n)
    }

    /// View: latest single observation `(price, ts)`. Convenience for
    /// indexers; protocol logic should prefer `get_twap`.
    pub fn latest_observation(env: Env) -> Result<(i128, u64), Error> {
        let count: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::ObservationCount)
            .unwrap_or(0);
        if count == 0 {
            return Err(Error::OracleStale);
        }
        let cfg = read_config(&env);
        let head: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::ObservationHead)
            .unwrap_or(0);
        // Newest is at `(head - 1) mod n`.
        let n = cfg.observation_window;
        let newest = (head + n - 1) % n;
        let obs: Observation = env
            .storage()
            .persistent()
            .get(&DataKey::Observations(newest))
            .ok_or(Error::OracleStale)?;
        Ok((obs.price, obs.ts))
    }

    /// View: whether the oracle is frozen.
    pub fn is_frozen(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::Frozen)
            .unwrap_or(false)
    }

    /// View: whether the oracle is paused.
    pub fn is_paused(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false)
    }

    /// Admin-only: pause the oracle. Instant. While paused, all sync /
    /// read / observation paths revert with `Paused`.
    pub fn pause(env: Env) -> Result<(), Error> {
        require_admin(&env)?;
        env.storage().instance().set(&DataKey::Paused, &true);
        env.events().publish((TOPIC_PAUSE, symbol_short!("on")), ());
        Ok(())
    }

    /// Admin-only: propose unpause. Records a 48h-eta proposal; the
    /// asymmetric pause from ADR-002 forbids instant unpause.
    pub fn propose_unpause(env: Env) -> Result<(), Error> {
        require_admin(&env)?;
        let now = env.ledger().timestamp();
        let pending = PendingUnpause {
            eta: now + REATTEST_TIMELOCK_SECONDS,
        };
        env.storage()
            .instance()
            .set(&DataKey::PendingUnpause, &pending);
        Ok(())
    }

    /// Permissionless after eta: finalize the pending unpause.
    pub fn execute_unpause(env: Env) -> Result<(), Error> {
        let pending: PendingUnpause = env
            .storage()
            .instance()
            .get(&DataKey::PendingUnpause)
            .ok_or(Error::NotInitialized)?;
        let now = env.ledger().timestamp();
        if now < pending.eta {
            return Err(Error::Unauthorized);
        }
        env.storage().instance().set(&DataKey::Paused, &false);
        env.storage().instance().remove(&DataKey::PendingUnpause);
        env.events().publish((TOPIC_PAUSE, symbol_short!("off")), ());
        Ok(())
    }

    /// Admin-only: propose a re-attestation. Records `(attested_rate,
    /// eta = now + 48h)`. After the eta, `execute_reattest` finalises
    /// the operation, writes the attested rate as the new cached rate,
    /// and clears the frozen flag.
    ///
    /// Repeated calls overwrite the pending proposal (and reset the
    /// timer), so the admin can revise the attested rate before
    /// execution. This is desirable: between proposal and execution, the
    /// admin may observe Blend's true rate moving and want to update.
    pub fn reattest(env: Env, attested_rate: i128) -> Result<(), Error> {
        require_admin(&env)?;
        if !Self::is_frozen(env.clone()) {
            // Reattest only makes sense while frozen — calling it
            // otherwise is almost certainly an admin mistake.
            return Err(Error::OracleRejected);
        }
        if attested_rate <= 0 {
            return Err(Error::OracleRejected);
        }
        let now = env.ledger().timestamp();
        let pending = PendingReattest {
            rate: attested_rate,
            eta: now + REATTEST_TIMELOCK_SECONDS,
        };
        env.storage()
            .instance()
            .set(&DataKey::PendingReattest, &pending);
        Ok(())
    }

    /// Cancel a pending reattest proposal. Admin-only.
    pub fn cancel_reattest(env: Env) -> Result<(), Error> {
        require_admin(&env)?;
        env.storage().instance().remove(&DataKey::PendingReattest);
        Ok(())
    }

    /// Permissionless: execute a previously-proposed reattest after its
    /// `eta`. Unfreezes the oracle and overwrites the cached rate.
    pub fn execute_reattest(env: Env) -> Result<i128, Error> {
        let pending: PendingReattest = env
            .storage()
            .instance()
            .get(&DataKey::PendingReattest)
            .ok_or(Error::NotInitialized)?;
        let now = env.ledger().timestamp();
        if now < pending.eta {
            return Err(Error::Unauthorized);
        }
        // Apply.
        write_cached_rate(&env, pending.rate, now);
        env.storage().instance().set(&DataKey::Frozen, &false);
        env.storage().instance().remove(&DataKey::PendingReattest);

        env.events()
            .publish((TOPIC_REATTEST,), (pending.rate, now));
        Ok(pending.rate)
    }

    // ---------------------------------------------------------------------
    // Read-only helpers (useful for tests + indexers).
    // ---------------------------------------------------------------------

    /// View: the active config.
    pub fn config(env: Env) -> OracleConfig {
        read_config(&env)
    }

    /// View: the current admin address.
    pub fn admin(env: Env) -> Address {
        read_admin(&env)
    }

    /// View: the cached rate and its timestamp without applying the
    /// staleness gate. Returns `(0, 0)` if never synced.
    pub fn cached_rate_raw(env: Env) -> (i128, u64) {
        (read_cached_rate(&env), read_cached_at(&env))
    }

    /// View: how many observations are currently populated.
    pub fn observation_count(env: Env) -> u32 {
        env.storage()
            .persistent()
            .get(&DataKey::ObservationCount)
            .unwrap_or(0)
    }
}

// -------------------------------------------------------------------------
// Helpers.
// -------------------------------------------------------------------------

/// Reason a freeze was triggered. Encoded as a `Symbol` in events so
/// indexers can switch on it without parsing string payloads.
enum FreezeReason {
    Monotonicity,
    Delta,
}

impl FreezeReason {
    fn tag(&self) -> Symbol {
        match self {
            Self::Monotonicity => symbol_short!("mono"),
            Self::Delta => symbol_short!("delta"),
        }
    }
}

fn validate_config(env: &Env, cfg: &OracleConfig) {
    let bad = cfg.max_delta_per_sync_wad <= 0
        || cfg.max_delta_per_sync_wad >= WAD
        || cfg.observation_window < 2
        || cfg.max_staleness_ledgers == 0;
    if bad {
        panic_with_error!(env, Error::OracleRejected);
    }
}

fn instance_has(env: &Env, key: &DataKey) -> bool {
    env.storage().instance().has(key)
}

fn read_config(env: &Env) -> OracleConfig {
    env.storage()
        .instance()
        .get(&DataKey::Config)
        .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized))
}

fn read_admin(env: &Env) -> Address {
    env.storage()
        .instance()
        .get(&DataKey::Admin)
        .unwrap_or_else(|| panic_with_error!(env, Error::NotInitialized))
}

fn require_admin(env: &Env) -> Result<(), Error> {
    let admin = read_admin(env);
    admin.require_auth();
    Ok(())
}

fn check_not_paused(env: &Env) -> Result<(), Error> {
    let paused: bool = env
        .storage()
        .instance()
        .get(&DataKey::Paused)
        .unwrap_or(false);
    if paused {
        return Err(Error::Paused);
    }
    Ok(())
}

fn check_not_frozen(env: &Env) -> Result<(), Error> {
    let frozen: bool = env
        .storage()
        .instance()
        .get(&DataKey::Frozen)
        .unwrap_or(false);
    if frozen {
        return Err(Error::OracleRejected);
    }
    Ok(())
}

fn read_cached_rate(env: &Env) -> i128 {
    env.storage()
        .persistent()
        .get(&DataKey::CachedRate)
        .unwrap_or(0)
}

fn read_cached_at(env: &Env) -> u64 {
    env.storage()
        .persistent()
        .get(&DataKey::CachedRateAt)
        .unwrap_or(0)
}

fn write_cached_rate(env: &Env, new_rate: i128, now: u64) {
    env.storage().persistent().set(&DataKey::CachedRate, &new_rate);
    env.storage()
        .persistent()
        .set(&DataKey::CachedRateAt, &now);
    bump_persistent(env, &DataKey::CachedRate);
    bump_persistent(env, &DataKey::CachedRateAt);
}

fn bump_persistent(env: &Env, key: &DataKey) {
    env.storage()
        .persistent()
        .extend_ttl(key, TTL_BUMP_THRESHOLD, EXTENDED_TTL);
}

fn freeze(env: &Env) {
    env.storage().instance().set(&DataKey::Frozen, &true);
}

fn fetch_blend_rate(env: &Env, blend_pool: &Address) -> Result<i128, Error> {
    let client = BlendPoolClient::new(env, blend_pool);
    let data = client.get_reserve_data();
    if data.b_rate <= 0 {
        return Err(Error::OracleRejected);
    }
    // Re-scale 1e9 -> WAD (1e18). At i128 width this multiplication
    // cannot overflow for any plausible Blend rate (Blend rates live in
    // [1, ~1e12] in 1e9 scaling).
    data.b_rate
        .checked_mul(BLEND_TO_WAD_SCALE)
        .ok_or(Error::MathOverflow)
}

fn emit_freeze(env: &Env, reason: FreezeReason, attempted: i128, cached: i128) {
    env.events()
        .publish((TOPIC_FROZEN, reason.tag()), (attempted, cached));
}

fn emit_index_sync(env: &Env, new_rate: i128, old_rate: i128, at: u64) {
    env.events().publish(
        (TOPIC_INDEX,),
        IndexSyncEvent {
            new_index: new_rate,
            old_index: old_rate,
            at_timestamp: at,
        },
    );
}

// -------------------------------------------------------------------------
// TWAP computation.
// -------------------------------------------------------------------------

/// Compute the time-weighted average price across `len` observations
/// starting at slot `start` and wrapping modulo `n`. Returns
/// `OracleStale` if `len < 2` or the spanned time window is zero.
///
/// Formula: with samples `(p[0], t[0]), ..., (p[len-1], t[len-1])`
/// sorted oldest-to-newest,
///
/// ```text
///     TWAP = Σ p[i] * (t[i+1] - t[i])   for i in 0..len-1
///            ────────────────────────
///                  t[len-1] - t[0]
/// ```
///
/// Each sample is held responsible for the *forward* duration it
/// represents (until the next sample replaces it). The newest sample is
/// excluded from the numerator because we have no "next sample"
/// duration for it yet — using `now` would let the AMM manipulate the
/// just-written value, which violates the read-previous-TWAP rule from
/// ADR-001 §4.
fn compute_twap(env: &Env, start: u32, len: u32, n: u32) -> Result<i128, Error> {
    if len < 2 {
        return Err(Error::OracleStale);
    }
    // Read all samples into stack-local pair of (i128, u64).
    let mut prev_obs: Option<Observation> = None;
    let mut weighted_sum_lo: i128 = 0; // overflow-checked accumulator
    let mut total_dt: i128 = 0;

    let mut first_ts: u64 = 0;
    let mut last_ts: u64 = 0;

    for i in 0..len {
        let slot = (start + i) % n;
        let obs: Observation = env
            .storage()
            .persistent()
            .get(&DataKey::Observations(slot))
            .ok_or(Error::OracleStale)?;
        if i == 0 {
            first_ts = obs.ts;
        }
        last_ts = obs.ts;
        if let Some(prev) = prev_obs.as_ref() {
            // Sanity: timestamps must be non-decreasing.
            if obs.ts < prev.ts {
                return Err(Error::OracleRejected);
            }
            let dt_u = obs.ts - prev.ts;
            let dt = i128::from(dt_u);
            // weight previous price by the dt it held.
            let term = prev
                .price
                .checked_mul(dt)
                .ok_or(Error::MathOverflow)?;
            weighted_sum_lo = weighted_sum_lo
                .checked_add(term)
                .ok_or(Error::MathOverflow)?;
            total_dt = total_dt.checked_add(dt).ok_or(Error::MathOverflow)?;
        }
        prev_obs = Some(obs);
    }

    if total_dt == 0 || last_ts == first_ts {
        return Err(Error::OracleStale);
    }
    // Division: weighted_sum / total_dt. Both are i128.
    let twap = weighted_sum_lo
        .checked_div(total_dt)
        .ok_or(Error::DivisionByZero)?;
    Ok(twap)
}
