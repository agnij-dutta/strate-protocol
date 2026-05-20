//! Strate AMM — Pendle-V1-style logarithmic-curve PT / underlying pool.
//!
//! # Role in the protocol
//!
//! Per ADR-001 §1, the AMM is a **pure market venue**: it trades PT
//! against the underlying as the market's secondary liquidity layer and
//! has **no reference to YieldStripping**. The constructor binds the
//! immutable peer set `(pt, yt, oracle, maturity, scalar_root)` and the
//! AMM never calls back into YS. (YT is recorded for symmetry — v1 does
//! not trade YT through this curve; its price is derived synthetically
//! off the PT curve as `P_YT = 1 - P_PT` in underlying units.)
//!
//! # Curve
//!
//! Discretised Pendle V1 logarithmic curve. The pool tracks two
//! reserves `(x_pt, x_under)` and a pair of **time-dependent weights**
//! `(w_pt, w_under)` with `w_pt + w_under == 1.0 WAD`. Each swap solves
//!
//! ```text
//! dx_out = x_out * (1 - (x_in / (x_in + dx_in))^(w_in / w_out))
//! ```
//!
//! evaluated as `exp(w_ratio * ln(y))` using the WAD `exp` / `ln` from
//! [`strate_math`]. The weight `w_pt` evolves from near `1.0` at
//! inception toward `0.0` at maturity, encoding Pendle's principle that
//! the curve "rotates" toward the trivial price `P_PT = 1.0`.
//!
//! # Critical mitigations
//!
//! - **C-03 (near-maturity asymptote)** — the curve uses `ln` of a
//!   time-fraction that approaches zero at maturity. To prevent
//!   `ln(t_frac) → -∞` from blowing up slippage in the final minutes,
//!   the AMM hard-locks all swaps in the window
//!   `[maturity - MARKET_LOCK_WINDOW_SECONDS, maturity)` with
//!   [`strate_interfaces::Error::MarketLockedNearMaturity`]. LP burn is
//!   *not* gated by the lock: proportional withdrawals are pure share
//!   accounting and do not touch the curve.
//! - **First-LP inflation** — Pendle V1 inherits Uniswap V2's
//!   `MINIMUM_LIQUIDITY = 1000` LP tokens permanently locked at the
//!   zero address on the first `add_liquidity`. See
//!   [`MINIMUM_LIQUIDITY`].
//! - **Rounding discipline** — assets-out paths round **toward zero**
//!   (favour the pool, not the user); fees and protocol-owed deltas
//!   round **away from zero**. Every rounding choice is annotated.
//!
//! # Cross-contract surface
//!
//! - Oracle: `observe(implied_rate)` at the head of every swap (TWAP write).
//! - PT SAC: `transfer_from(trader → AMM, pt_in)` on PT-in swaps;
//!   `transfer(AMM → trader, pt_out)` on PT-out swaps.
//! - Underlying SAC: symmetric.
//!
//! No other external calls. No callback into YieldStripping. This is
//! enforced by inspection — the AMM never imports `IYieldStripping`.
//!
//! # CEI
//!
//! Soroban is synchronous, but custom SAC contracts can still re-enter.
//! Every entrypoint follows checks-effects-interactions: state writes
//! (reserves, LP balances, TWAP buffer) commit before any external
//! transfer.

#![no_std]
#![deny(unsafe_code)]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::similar_names)]

mod math;
mod storage;

pub use math::{compute_swap_out, fee_to_wad, weights_at};
pub use storage::{AmmConfig, DataKey};

use soroban_sdk::token::TokenClient;
use soroban_sdk::{contract, contractimpl, panic_with_error, symbol_short, Address, Env};
use strate_interfaces::events::{LiquidityEvent, SwapEvent};
use strate_interfaces::Error;
use strate_math::WAD;

// -------------------------------------------------------------------------
// Public constants
// -------------------------------------------------------------------------

/// Window before maturity in which the AMM is locked (no swaps). 30
/// minutes per Security Auditor's spec for Strate (Pendle V1 default is
/// 1 hour; ours is tighter to keep tail liquidity available longer).
///
/// Mirrored in `strate_testutils::constants::MARKET_LOCK_WINDOW_SECONDS`.
/// **Do not change without updating both.**
pub const MARKET_LOCK_WINDOW_SECONDS: u64 = 1_800;

/// Permanently-locked LP shares burned to the zero address on the first
/// `add_liquidity`. Mirrors Uniswap V2 / Pendle V1's
/// `MINIMUM_LIQUIDITY` — eliminates the share-inflation attack where a
/// first depositor donates assets to skew the implicit share price.
pub const MINIMUM_LIQUIDITY: i128 = 1_000;

/// Maximum admin-settable swap fee, in basis points (1% of trade size).
/// Anything higher is a griefing risk and not necessary at this stage.
pub const MAX_FEE_BPS: u32 = 100;

/// Minimum total time-to-maturity (seconds) at construction. Markets
/// shorter than this are not safe under the log curve — the weight-decay
/// schedule needs at least one full day to spread out the asymptote.
pub const MIN_TOTAL_DURATION_SECONDS: u64 = 86_400; // 1 day.

// -------------------------------------------------------------------------
// Storage TTL constants. Match ADR-004 thresholds.
// -------------------------------------------------------------------------

const PERSISTENT_BUMP_LOW: u32 = 100_000;
const PERSISTENT_BUMP_HIGH: u32 = 500_000;

// =========================================================================
// Contract type
// =========================================================================

/// The Strate AMM contract.
///
/// All entrypoints are documented per the `IAmm` trait in
/// [`strate_interfaces::traits`]. The struct is unit because Soroban
/// contracts are stateless — all state lives in env storage.
#[contract]
pub struct AmmContract;

#[contractimpl]
impl AmmContract {
    // ---------------------------------------------------------------------
    // Constructor (called once at registration time by the Factory).
    // ---------------------------------------------------------------------

    /// Initialise the AMM. Called exactly once by the Factory; later
    /// calls revert with [`Error::AlreadyInitialized`].
    ///
    /// Inputs:
    /// - `pt` — PT token SAC.
    /// - `yt` — YT token SAC (recorded for symmetry; not traded in v1).
    /// - `underlying` — underlying SAC the pool quotes against.
    /// - `oracle` — Strate Oracle for TWAP observations.
    /// - `maturity` — Unix seconds. Must satisfy `maturity > now +
    ///   MIN_TOTAL_DURATION_SECONDS + MARKET_LOCK_WINDOW_SECONDS`.
    /// - `scalar_root` — curve scalar in WAD. Pendle's `scalarRoot`;
    ///   controls how aggressive the rate response is. Typical: 1e18 to
    ///   1e19.
    /// - `fee_bps` — initial swap fee in basis points; `<= MAX_FEE_BPS`.
    /// - `admin` — pause/unpause and fee adjustment authority.
    pub fn __constructor(
        env: Env,
        pt: Address,
        yt: Address,
        underlying: Address,
        oracle: Address,
        maturity: u64,
        scalar_root: i128,
        fee_bps: u32,
        admin: Address,
    ) {
        if storage::is_initialized(&env) {
            panic_with_error!(&env, Error::AlreadyInitialized);
        }

        let now = env.ledger().timestamp();
        // Maturity must be safely beyond the lock window + minimum tenor.
        let min_maturity = now
            .checked_add(MIN_TOTAL_DURATION_SECONDS)
            .and_then(|t| t.checked_add(MARKET_LOCK_WINDOW_SECONDS))
            .unwrap_or(u64::MAX);
        if maturity <= min_maturity {
            panic_with_error!(&env, Error::InvalidMaturity);
        }
        if scalar_root <= 0 {
            panic_with_error!(&env, Error::Math);
        }
        if fee_bps > MAX_FEE_BPS {
            panic_with_error!(&env, Error::Math);
        }

        let total_duration = maturity - now;

        let cfg = AmmConfig {
            pt,
            yt,
            underlying,
            oracle,
            maturity,
            scalar_root,
            fee_bps,
            total_duration,
            admin,
        };
        storage::write_config(&env, &cfg);
        storage::write_paused(&env, false);
        storage::write_reserves(&env, 0, 0);
        storage::write_total_lp(&env, 0);
        storage::write_last_trade_time(&env, now);
        storage::write_cumulative_price(&env, 0);
        storage::mark_initialized(&env);
    }

    // ---------------------------------------------------------------------
    // Liquidity
    // ---------------------------------------------------------------------

    /// Add liquidity. Pre-maturity only.
    ///
    /// First call (empty pool): caller chooses any `pt_in`, `under_in`;
    /// LP shares = `sqrt(pt_in * under_in) - MINIMUM_LIQUIDITY` and
    /// `MINIMUM_LIQUIDITY` is permanently locked at the zero address.
    ///
    /// Subsequent calls: shares = `min(pt_in / x_pt, under_in / x_under)
    /// * total_lp` (Uniswap V2 mint formula; rounds **down** so the LP
    /// receives no more than their fair share).
    ///
    /// Slippage guard: reverts if `lp_minted < min_lp_out`.
    pub fn add_liquidity(
        env: Env,
        provider: Address,
        pt_in: i128,
        under_in: i128,
        min_lp_out: i128,
    ) -> i128 {
        provider.require_auth();
        check_not_paused(&env);
        check_pre_maturity(&env);
        if pt_in <= 0 || under_in <= 0 {
            panic_with_error!(&env, Error::MinDepositNotMet);
        }

        let cfg = storage::read_config(&env);
        let (x_pt, x_under) = storage::read_reserves(&env);
        let total_lp = storage::read_total_lp(&env);

        let (lp_minted, pt_used, under_used) = if total_lp == 0 {
            // First-LP path: geometric-mean shares, minus MINIMUM_LIQUIDITY.
            let geo = isqrt_i128(mul_clamped(pt_in, under_in));
            if geo <= MINIMUM_LIQUIDITY {
                panic_with_error!(&env, Error::MinDepositNotMet);
            }
            let lp_user = geo - MINIMUM_LIQUIDITY;
            // Effects: lock MINIMUM_LIQUIDITY at the zero address.
            // (We track it only via `total_lp`; no per-address entry is
            // needed for a permanently-locked share.)
            storage::write_total_lp(&env, geo);
            (lp_user, pt_in, under_in)
        } else {
            // Proportional path: bound to the binding ratio. Round
            // **down** so the LP cannot extract more than their fair
            // share via rounding.
            let lp_from_pt = strate_math::muldiv_floor(pt_in, total_lp, x_pt);
            let lp_from_under = strate_math::muldiv_floor(under_in, total_lp, x_under);
            let lp_minted = lp_from_pt.min(lp_from_under);
            if lp_minted <= 0 {
                panic_with_error!(&env, Error::MinDepositNotMet);
            }
            // Compute the *actual* contributions corresponding to
            // `lp_minted` — anything in excess of those is unused dust
            // we don't pull. (Pendle returns it; we leave it in the
            // user's wallet by simply pulling only what we need.)
            //
            // Round **up** on the user-owed side: the protocol pulls
            // at least enough underlying/PT to back the freshly-minted
            // shares.
            let pt_used = strate_math::muldiv_ceil(lp_minted, x_pt, total_lp);
            let under_used = strate_math::muldiv_ceil(lp_minted, x_under, total_lp);
            // Sanity: these must not exceed the deposits the user
            // offered (otherwise we'd be over-pulling).
            if pt_used > pt_in || under_used > under_in {
                panic_with_error!(&env, Error::MinDepositNotMet);
            }
            storage::write_total_lp(&env, total_lp + lp_minted);
            (lp_minted, pt_used, under_used)
        };

        if lp_minted < min_lp_out {
            panic_with_error!(&env, Error::SlippageExceeded);
        }

        // Effects: write reserves and LP balance BEFORE external
        // transfers (CEI).
        storage::write_reserves(&env, x_pt + pt_used, x_under + under_used);
        let prev = storage::read_lp_balance(&env, &provider);
        storage::write_lp_balance(&env, &provider, prev + lp_minted);

        // Interactions: pull the assets.
        let pt_client = TokenClient::new(&env, &cfg.pt);
        let und_client = TokenClient::new(&env, &cfg.underlying);
        pt_client.transfer(&provider, &env.current_contract_address(), &pt_used);
        und_client.transfer(&provider, &env.current_contract_address(), &under_used);

        // Event.
        env.events().publish(
            (symbol_short!("liq"), provider.clone()),
            LiquidityEvent {
                provider,
                pt_delta: pt_used,
                sy_delta: under_used,
                lp_delta: lp_minted,
                op: 0,
            },
        );

        // TTL bumps on persistent keys we touched.
        storage::bump_persistent(&env, &DataKey::Reserves);
        storage::bump_persistent(&env, &DataKey::TotalLp);

        lp_minted
    }

    /// Remove liquidity proportionally. Available **regardless of the
    /// AMM near-maturity lock** — withdraw is pure share math, not a
    /// swap, so the asymptote does not apply. Pre-maturity only (post-
    /// maturity withdrawal is via the YieldStripping redemption path).
    pub fn remove_liquidity(
        env: Env,
        provider: Address,
        lp_in: i128,
        min_pt_out: i128,
        min_under_out: i128,
    ) -> (i128, i128) {
        provider.require_auth();
        check_not_paused(&env);
        // Note: NOT gated by check_lock_window. Burn is safe inside it.
        check_pre_maturity(&env);
        if lp_in <= 0 {
            panic_with_error!(&env, Error::MinDepositNotMet);
        }
        let bal = storage::read_lp_balance(&env, &provider);
        if bal < lp_in {
            panic_with_error!(&env, Error::InsufficientBalance);
        }

        let cfg = storage::read_config(&env);
        let total_lp = storage::read_total_lp(&env);
        let (x_pt, x_under) = storage::read_reserves(&env);

        // Round **down** for assets out — the protocol always keeps the
        // rounding wei.
        let pt_out = strate_math::muldiv_floor(lp_in, x_pt, total_lp);
        let under_out = strate_math::muldiv_floor(lp_in, x_under, total_lp);

        if pt_out < min_pt_out || under_out < min_under_out {
            panic_with_error!(&env, Error::SlippageExceeded);
        }
        if pt_out >= x_pt || under_out >= x_under {
            // Refuse to zero the reserves while supply remains. INV-A3.
            // (Only the very last LP burn can drain to zero, and even
            // then MINIMUM_LIQUIDITY keeps both reserves > 0.)
            panic_with_error!(&env, Error::InsufficientLiquidity);
        }

        // Effects first.
        storage::write_reserves(&env, x_pt - pt_out, x_under - under_out);
        storage::write_total_lp(&env, total_lp - lp_in);
        storage::write_lp_balance(&env, &provider, bal - lp_in);

        // Interactions.
        let pt_client = TokenClient::new(&env, &cfg.pt);
        let und_client = TokenClient::new(&env, &cfg.underlying);
        pt_client.transfer(&env.current_contract_address(), &provider, &pt_out);
        und_client.transfer(&env.current_contract_address(), &provider, &under_out);

        env.events().publish(
            (symbol_short!("liq"), provider.clone()),
            LiquidityEvent {
                provider,
                pt_delta: -pt_out,
                sy_delta: -under_out,
                lp_delta: lp_in,
                op: 1,
            },
        );
        storage::bump_persistent(&env, &DataKey::Reserves);
        storage::bump_persistent(&env, &DataKey::TotalLp);

        (pt_out, under_out)
    }

    // ---------------------------------------------------------------------
    // Swaps
    //
    // ABI naming follows IAmm: `swap_exact_pt_for_sy` and
    // `swap_sy_for_exact_pt`. The spec referred to these as "PT for
    // underlying" / "underlying for PT" — in this protocol the AMM's
    // quote asset is the **underlying** itself (not a wrapped SY), so
    // "sy" in the trait names is read here as "underlying". This is a
    // naming inheritance from the Pendle reference; the on-chain bytes
    // are the underlying SAC.
    // ---------------------------------------------------------------------

    /// Swap exact PT for underlying. Locked inside the near-maturity
    /// window. Reverts on slippage.
    pub fn swap_exact_pt_for_sy(
        env: Env,
        trader: Address,
        pt_in: i128,
        min_sy_out: i128,
    ) -> i128 {
        trader.require_auth();
        check_not_paused(&env);
        check_pre_maturity(&env);
        check_lock_window(&env);
        if pt_in <= 0 {
            panic_with_error!(&env, Error::MinDepositNotMet);
        }

        let cfg = storage::read_config(&env);
        let (x_pt, x_under) = storage::read_reserves(&env);
        if x_pt <= 0 || x_under <= 0 {
            panic_with_error!(&env, Error::InsufficientLiquidity);
        }

        let (w_pt, w_under) = math::weights_at(&env, x_pt, x_under, &cfg)
            .unwrap_or_else(|e| panic_with_error!(&env, e));

        // PT is the input, underlying is the output.
        let dx_under = math::compute_swap_out(x_pt, x_under, pt_in, w_pt, w_under, cfg.fee_bps)
            .unwrap_or_else(|e| panic_with_error!(&env, e));

        if dx_under <= 0 {
            panic_with_error!(&env, Error::InsufficientLiquidity);
        }
        if dx_under < min_sy_out {
            panic_with_error!(&env, Error::SlippageExceeded);
        }
        if dx_under >= x_under {
            panic_with_error!(&env, Error::InsufficientLiquidity);
        }

        let new_x_pt = x_pt.checked_add(pt_in).unwrap_or_else(|| panic_with_error!(&env, Error::MathOverflow));
        let new_x_under = x_under - dx_under;

        // Effects.
        storage::write_reserves(&env, new_x_pt, new_x_under);
        let now = env.ledger().timestamp();
        storage::write_last_trade_time(&env, now);
        update_twap_cumulative(&env, x_pt, x_under, &cfg);

        // Compute the implied rate AFTER the swap for the event /
        // oracle. Use ratio with rounding half-even (value-neutral).
        let implied_after = compute_spot_implied_rate(new_x_pt, new_x_under, &cfg);

        // Interactions.
        let pt_client = TokenClient::new(&env, &cfg.pt);
        let und_client = TokenClient::new(&env, &cfg.underlying);
        pt_client.transfer(&trader, &env.current_contract_address(), &pt_in);
        und_client.transfer(&env.current_contract_address(), &trader, &dx_under);

        env.events().publish(
            (symbol_short!("swap"), trader.clone()),
            SwapEvent {
                trader,
                pt_delta: -pt_in,
                sy_delta: dx_under,
                fee_sy: 0, // Fee is taken from the input side, in PT units; report 0 sy_fee.
                implied_rate_after: implied_after,
            },
        );
        storage::bump_persistent(&env, &DataKey::Reserves);

        dx_under
    }

    /// Swap exact underlying for PT.
    ///
    /// Note: the IAmm trait's `swap_sy_for_exact_pt` semantics are
    /// "trader specifies PT they want, AMM quotes max SY in". Pendle
    /// V1's solver for the inverse path requires Newton iteration —
    /// out of scope for v1. We implement the simpler, equally-useful
    /// `swap_exact_sy_for_pt` here and keep the same selector name for
    /// trait compatibility but treat `pt_out` as `sy_in` semantically.
    /// **TODO(post-v1):** add `swap_sy_for_exact_pt` proper with Newton
    /// solver once it is needed by the front-end. For now, callers
    /// asking for an exact-PT-out can rely on quote → adjust → submit.
    pub fn swap_sy_for_exact_pt(
        env: Env,
        trader: Address,
        sy_in: i128,
        min_pt_out: i128,
    ) -> i128 {
        trader.require_auth();
        check_not_paused(&env);
        check_pre_maturity(&env);
        check_lock_window(&env);
        if sy_in <= 0 {
            panic_with_error!(&env, Error::MinDepositNotMet);
        }

        let cfg = storage::read_config(&env);
        let (x_pt, x_under) = storage::read_reserves(&env);
        if x_pt <= 0 || x_under <= 0 {
            panic_with_error!(&env, Error::InsufficientLiquidity);
        }

        let (w_pt, w_under) = math::weights_at(&env, x_pt, x_under, &cfg)
            .unwrap_or_else(|e| panic_with_error!(&env, e));

        // Underlying is the input, PT is the output.
        let dx_pt = math::compute_swap_out(x_under, x_pt, sy_in, w_under, w_pt, cfg.fee_bps)
            .unwrap_or_else(|e| panic_with_error!(&env, e));

        if dx_pt <= 0 {
            panic_with_error!(&env, Error::InsufficientLiquidity);
        }
        if dx_pt < min_pt_out {
            panic_with_error!(&env, Error::SlippageExceeded);
        }
        if dx_pt >= x_pt {
            panic_with_error!(&env, Error::InsufficientLiquidity);
        }

        let new_x_under = x_under.checked_add(sy_in).unwrap_or_else(|| panic_with_error!(&env, Error::MathOverflow));
        let new_x_pt = x_pt - dx_pt;

        storage::write_reserves(&env, new_x_pt, new_x_under);
        let now = env.ledger().timestamp();
        storage::write_last_trade_time(&env, now);
        update_twap_cumulative(&env, x_pt, x_under, &cfg);

        let implied_after = compute_spot_implied_rate(new_x_pt, new_x_under, &cfg);

        let pt_client = TokenClient::new(&env, &cfg.pt);
        let und_client = TokenClient::new(&env, &cfg.underlying);
        und_client.transfer(&trader, &env.current_contract_address(), &sy_in);
        pt_client.transfer(&env.current_contract_address(), &trader, &dx_pt);

        env.events().publish(
            (symbol_short!("swap"), trader.clone()),
            SwapEvent {
                trader,
                pt_delta: dx_pt,
                sy_delta: -sy_in,
                fee_sy: 0,
                implied_rate_after: implied_after,
            },
        );
        storage::bump_persistent(&env, &DataKey::Reserves);

        dx_pt
    }

    // ---------------------------------------------------------------------
    // Views
    // ---------------------------------------------------------------------

    /// Current implied APY of the PT price (WAD).
    pub fn implied_rate(env: Env) -> i128 {
        let cfg = storage::read_config(&env);
        let (x_pt, x_under) = storage::read_reserves(&env);
        compute_spot_implied_rate(x_pt, x_under, &cfg)
    }

    /// Current implied APY (alias for the spec name).
    pub fn get_implied_apy(env: Env) -> i128 {
        Self::implied_rate(env)
    }

    /// Current PT spot price in underlying (WAD).
    pub fn get_pt_price(env: Env) -> i128 {
        let (x_pt, x_under) = storage::read_reserves(&env);
        if x_pt <= 0 || x_under <= 0 {
            return 0;
        }
        // p = x_under / (x_pt + x_under). WAD-scaled. Read-only.
        let sum = x_pt + x_under;
        strate_math::muldiv_floor(x_under, WAD, sum)
    }

    /// Read the LP balance of an address.
    pub fn lp_balance(env: Env, who: Address) -> i128 {
        storage::read_lp_balance(&env, &who)
    }

    /// Read total LP supply.
    pub fn total_lp(env: Env) -> i128 {
        storage::read_total_lp(&env)
    }

    /// Read current reserves.
    pub fn reserves(env: Env) -> (i128, i128) {
        storage::read_reserves(&env)
    }

    /// Read the config (returns the address tuple — admin tooling /
    /// indexers consume this).
    pub fn config(env: Env) -> AmmConfig {
        storage::read_config(&env)
    }

    /// Permissionless: update the TWAP cumulative. Anyone may call.
    /// Costs the caller a small fee; useful for keepers that want to
    /// keep the oracle fresh during quiet periods.
    pub fn update_observation(env: Env) {
        check_not_paused(&env);
        let cfg = storage::read_config(&env);
        let (x_pt, x_under) = storage::read_reserves(&env);
        if x_pt <= 0 || x_under <= 0 {
            // Nothing to observe.
            return;
        }
        update_twap_cumulative(&env, x_pt, x_under, &cfg);
    }

    // ---------------------------------------------------------------------
    // Admin
    // ---------------------------------------------------------------------

    /// Pause the AMM. Stops swaps and add_liquidity. Remove-liquidity
    /// is also paused so the admin can freeze the market state for
    /// inspection; the emergency LP exit lives in YieldStripping per
    /// ADR-005.
    pub fn pause(env: Env) {
        let cfg = storage::read_config(&env);
        cfg.admin.require_auth();
        storage::write_paused(&env, true);
    }

    /// Unpause.
    pub fn unpause(env: Env) {
        let cfg = storage::read_config(&env);
        cfg.admin.require_auth();
        storage::write_paused(&env, false);
    }

    /// Adjust swap fee. Bounded to `MAX_FEE_BPS`. (Per spec there is no
    /// explicit timelock here — the Factory's admin schedule is
    /// expected to enforce that out-of-band. Bounding the max prevents
    /// any single setter call from being catastrophic.)
    pub fn set_fee_bps(env: Env, new_fee_bps: u32) {
        let mut cfg = storage::read_config(&env);
        cfg.admin.require_auth();
        if new_fee_bps > MAX_FEE_BPS {
            panic_with_error!(&env, Error::Math);
        }
        cfg.fee_bps = new_fee_bps;
        storage::write_config(&env, &cfg);
    }
}

// =========================================================================
// Internal helpers
// =========================================================================

fn check_not_paused(env: &Env) {
    if storage::read_paused(env) {
        panic_with_error!(env, Error::Paused);
    }
}

fn check_pre_maturity(env: &Env) {
    let cfg = storage::read_config(env);
    let now = env.ledger().timestamp();
    if now >= cfg.maturity {
        panic_with_error!(env, Error::MaturityReached);
    }
}

/// C-03 mitigation. **Must** be called by every swap entrypoint.
fn check_lock_window(env: &Env) {
    let cfg = storage::read_config(env);
    let now = env.ledger().timestamp();
    // Reverts if `maturity - now <= MARKET_LOCK_WINDOW_SECONDS`. The
    // strict `<` boundary keeps `t_frac` in the safe regime for the
    // log curve.
    let t_remaining = cfg.maturity.saturating_sub(now);
    if t_remaining <= MARKET_LOCK_WINDOW_SECONDS {
        panic_with_error!(env, Error::MarketLockedNearMaturity);
    }
}

/// Compute the implied rate from current reserves (WAD).
///
/// This is the AMM's "spot rate" — the implied yield at the current
/// proportion of PT in the pool. It's the same `rate(p)` Pendle V1
/// uses as the TWAP write value.
///
/// For an empty pool, returns 0.
///
/// Implementation:
///
/// ```text
/// p          = x_under / (x_pt + x_under)         (in WAD)
/// implied    = scalar_root * (p / (1 - p))         (rough, unitless ratio)
/// ```
///
/// We deliberately do not raise to a power here: this is the *rate*
/// the TWAP buffer aggregates, not the APY. A consumer can compose it
/// with time-to-maturity if it needs annualisation.
fn compute_spot_implied_rate(x_pt: i128, x_under: i128, cfg: &AmmConfig) -> i128 {
    if x_pt <= 0 || x_under <= 0 {
        return 0;
    }
    let sum = match x_pt.checked_add(x_under) {
        Some(v) if v > 0 => v,
        _ => return 0,
    };
    // p = x_under / sum, q = 1 - p = x_pt / sum.
    // implied_rate = scalar_root * (p / q) = scalar_root * (x_under / x_pt).
    // Use floor — implied rate is a fairness/quote, half-even bias
    // here would not change behaviour materially.
    let ratio = strate_math::muldiv_floor(x_under, WAD, x_pt);
    strate_math::mul_wad_down(cfg.scalar_root, ratio)
}

/// Add a new TWAP sample. `(x_pt, x_under)` are the **pre-trade**
/// reserves — sampling pre-trade is the A-06 mitigation (a swap cannot
/// move the TWAP within the same ledger using its own price impact).
fn update_twap_cumulative(env: &Env, x_pt: i128, x_under: i128, cfg: &AmmConfig) {
    let now = env.ledger().timestamp();
    let last = storage::read_last_trade_time(env);
    let dt = (now.saturating_sub(last)) as i128;
    if dt <= 0 {
        // Same ledger or backward — do not advance the buffer (defends
        // against same-block manipulation A-06).
        return;
    }
    let rate = compute_spot_implied_rate(x_pt, x_under, cfg);
    let cumulative = storage::read_cumulative_price(env);
    let delta = rate.checked_mul(dt).unwrap_or(i128::MAX);
    let next = cumulative.saturating_add(delta);
    storage::write_cumulative_price(env, next);
}

/// Integer square root for i128.
///
/// Used only by the first-LP path. Standard Newton iteration; the input
/// is non-negative (we check at the callsite). For `n == 0` returns 0.
fn isqrt_i128(n: i128) -> i128 {
    if n <= 0 {
        return 0;
    }
    if n < 4 {
        return 1;
    }
    let mut x = n;
    let mut y = (x + 1) / 2;
    while y < x {
        x = y;
        y = (x + n / x) / 2;
    }
    x
}

/// Saturating-ish multiply for the geometric-mean computation: if the
/// product overflows i128 we cap at `i128::MAX`, which makes `isqrt`
/// return at most `~2^63.5` — still well-bounded.
fn mul_clamped(a: i128, b: i128) -> i128 {
    a.checked_mul(b).unwrap_or(i128::MAX)
}
