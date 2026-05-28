//! Pure-signature traits exposed by each Strate contract.
//!
//! These are the public ABI surfaces. The concrete contract structs in
//! `contracts/*` `impl` them. Mock implementations in `strate-testutils`
//! also `impl` them so contract-vs-contract integration tests can swap
//! a real Blend oracle for a deterministic mock without touching the
//! caller's code.
//!
//! Trait methods take `Env` explicitly (rather than `&self`) because
//! Soroban contracts are unit structs with no instance state; the
//! environment is the universe.

use soroban_sdk::{Address, Env};

use crate::Error;

// -------------------------------------------------------------------------
// IYieldStripping — the mint/redeem/claim core contract.
// -------------------------------------------------------------------------

/// Core yield-stripping entry points. Implemented by the
/// `yield-stripping` contract.
pub trait IYieldStripping {
    /// Deposit underlying, mint matching PT and YT to `to`.
    ///
    /// Returns `(pt_minted, yt_minted, index_at_mint)`.
    fn mint(env: Env, from: Address, to: Address, amount: i128)
        -> Result<(i128, i128, i128), Error>;

    /// Pre-maturity: burn equal PT+YT, withdraw underlying. The user
    /// can also call this post-maturity but it's gas-inefficient
    /// because YT is worthless then.
    fn redeem_pair(env: Env, from: Address, amount: i128) -> Result<i128, Error>;

    /// Post-maturity: burn PT alone, withdraw underlying.
    fn redeem_pt(env: Env, from: Address, amount: i128) -> Result<i128, Error>;

    /// Claim accrued yield for `user`. Idempotent within a single
    /// ledger; rolls the global index forward as a side effect.
    fn claim_yield(env: Env, user: Address) -> Result<i128, Error>;

    /// Read-only: current global yield index in WAD.
    fn current_index(env: Env) -> Result<i128, Error>;

    /// Read-only: pending yield for `user` if they were to claim now.
    fn pending_yield(env: Env, user: Address) -> Result<i128, Error>;
}

// -------------------------------------------------------------------------
// IAmm — the Pendle-style logarithmic-curve AMM.
// -------------------------------------------------------------------------

/// AMM entry points. Implemented by the `amm` contract.
pub trait IAmm {
    /// Swap exact `pt_in` for SY, with `min_sy_out` slippage guard.
    fn swap_exact_pt_for_sy(
        env: Env,
        trader: Address,
        pt_in: i128,
        min_sy_out: i128,
    ) -> Result<i128, Error>;

    /// Swap SY for exact `pt_out`, capping SY in at `max_sy_in`.
    fn swap_sy_for_exact_pt(
        env: Env,
        trader: Address,
        pt_out: i128,
        max_sy_in: i128,
    ) -> Result<i128, Error>;

    /// Add liquidity at the current ratio. Returns LP tokens minted.
    fn add_liquidity(
        env: Env,
        provider: Address,
        pt_in: i128,
        sy_in: i128,
        min_lp_out: i128,
    ) -> Result<i128, Error>;

    /// Remove liquidity proportionally. Returns `(pt_out, sy_out)`.
    fn remove_liquidity(
        env: Env,
        provider: Address,
        lp_in: i128,
        min_pt_out: i128,
        min_sy_out: i128,
    ) -> Result<(i128, i128), Error>;

    /// Quote-only: implied rate (WAD) at current reserves.
    fn implied_rate(env: Env) -> Result<i128, Error>;
}

// -------------------------------------------------------------------------
// IOracle — TWAP wrapper over the AMM's implied rate, fed by Blend.
// -------------------------------------------------------------------------

/// Oracle entry points. Implemented by the `oracle` contract.
pub trait IOracle {
    /// Push a new observation (called by AMM after each state-changing
    /// op).
    fn observe(env: Env, implied_rate: i128) -> Result<(), Error>;

    /// Read the TWAP implied rate over the last `window_seconds`
    /// seconds.
    fn twap(env: Env, window_seconds: u64) -> Result<i128, Error>;

    /// Read the latest single observation.
    fn latest(env: Env) -> Result<(i128, u64), Error>;
}

// -------------------------------------------------------------------------
// IPtToken — SEP-41 PT.
// -------------------------------------------------------------------------

/// PT (Principal Token) entry points. Implemented by the `pt-token`
/// contract. Adheres to SEP-41; the additional methods below are the
/// privileged mint/burn used by the yield-stripping contract.
pub trait IPtToken {
    /// Privileged mint. Only callable by the parent yield-stripping
    /// contract.
    fn mint_to(env: Env, to: Address, amount: i128) -> Result<(), Error>;

    /// Privileged burn. Only callable by the parent yield-stripping
    /// contract.
    fn burn_from(env: Env, from: Address, amount: i128) -> Result<(), Error>;

    /// Maturity timestamp (Unix seconds).
    fn maturity(env: Env) -> Result<u64, Error>;
}

// -------------------------------------------------------------------------
// IYtToken — SEP-41 YT with per-holder yield-index accounting.
// -------------------------------------------------------------------------

/// YT (Yield Token) entry points. Implemented by the `yt-token`
/// contract. SEP-41 surface plus the yield-accounting hooks.
///
/// Post H-01 fix (see `docs/handoff/H-01-reentry-fix.md`), YT owns
/// the global yield index and per-user accrual state directly.
/// Soroban host 22.x forbids the cross-contract callback pattern
/// that the original design relied on; the push topology below
/// keeps YS and YT acyclic.
pub trait IYtToken {
    /// Privileged mint. The recipient is settled against the current
    /// global yield index *before* the balance increment.
    fn mint(env: Env, to: Address, amount: i128) -> Result<(), Error>;

    /// Privileged burn. The holder is settled *before* the balance
    /// decrement, so accrued yield is preserved into their drainable
    /// bucket.
    fn burn(env: Env, from: Address, amount: i128) -> Result<(), Error>;

    /// Read the current global yield index (WAD-scaled).
    fn global_yield_index(env: Env) -> Result<i128, Error>;

    /// Read the per-user yield-index snapshot (WAD-scaled). Returns 0
    /// for accounts that have never had a balance change.
    fn user_yield_index(env: Env, user: Address) -> Result<i128, Error>;

    /// Push a new global yield index. **Minter-only.** Enforces
    /// monotonicity (`new_index >= current`); a regression is
    /// rejected with `OracleRejected`.
    fn push_yield_index(env: Env, new_index: i128) -> Result<(), Error>;

    /// View-only: what `user` would receive on `drain_accrued` right
    /// now, including the not-yet-settled delta against the current
    /// global index.
    fn accrued_yield(env: Env, user: Address) -> Result<i128, Error>;

    /// **Minter-only.** Settle `user`'s pending delta, then zero
    /// and return their accrued-yield bucket. Atomic; the read of
    /// `AccruedYield` and the zero-write happen in the same call.
    fn drain_accrued(env: Env, user: Address) -> Result<i128, Error>;

    /// Maturity timestamp (Unix seconds).
    fn maturity(env: Env) -> Result<u64, Error>;
}

// -------------------------------------------------------------------------
// IFactory — deploys PT, YT, AMM per maturity.
// -------------------------------------------------------------------------

/// Factory entry points. Implemented by the `factory` contract.
pub trait IFactory {
    /// Deploy a new market (PT, YT, AMM trio) for `underlying` with
    /// the given `maturity`. Returns the three new contract addresses.
    fn deploy_market(
        env: Env,
        underlying: Address,
        maturity: u64,
    ) -> Result<(Address, Address, Address), Error>;

    /// Look up an existing market by `(underlying, maturity)`.
    fn get_market(
        env: Env,
        underlying: Address,
        maturity: u64,
    ) -> Result<(Address, Address, Address), Error>;
}
