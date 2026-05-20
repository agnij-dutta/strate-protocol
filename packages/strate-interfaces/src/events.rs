//! Typed event schemas.
//!
//! Each protocol event is documented as a `(topics, data)` tuple. The
//! topic vector goes first to identify the event uniquely on-chain;
//! the `data` struct carries the value-heavy fields. The constants
//! below are the `Symbol::short()` topic tags — contracts publish
//! using `env.events().publish((TAG, ...indexed...), data)`.
//!
//! ## Topic-tag conventions
//!
//! Symbol::short() limits us to <= 9 ASCII chars. We use the
//! canonical short forms below; any new event must add its tag
//! here as a `pub const` so all contracts share one source of truth.

use soroban_sdk::{contracttype, Address, Symbol};

// -------------------------------------------------------------------------
// Topic tag constants. Tags are mirrored verbatim by contracts via
// `soroban_sdk::symbol_short!(...)`.
// -------------------------------------------------------------------------

/// Topic tag for [`MintEvent`].
pub const TOPIC_MINT: &str = "mint";
/// Topic tag for [`RedeemEvent`] (both pre- and post-maturity).
pub const TOPIC_REDEEM: &str = "redeem";
/// Topic tag for [`YieldClaimedEvent`].
pub const TOPIC_YIELD: &str = "yield";
/// Topic tag for [`IndexSyncEvent`] — emitted whenever the yield
/// index is refreshed (mint / claim / redeem entry points).
pub const TOPIC_INDEX: &str = "indexsync";
/// Topic tag for [`SwapEvent`].
pub const TOPIC_SWAP: &str = "swap";
/// Topic tag for [`LiquidityEvent`] (mint or burn LP).
pub const TOPIC_LIQ: &str = "liquidity";
/// Topic tag for [`ObservationEvent`] (oracle TWAP write).
pub const TOPIC_OBS: &str = "observe";

// -------------------------------------------------------------------------
// Event payloads. These are the `data` arg of `events().publish`.
// -------------------------------------------------------------------------

/// `(TOPIC_MINT, user)` indexed; `data = MintEvent`.
#[contracttype]
#[derive(Clone, Debug)]
pub struct MintEvent {
    /// User receiving PT + YT.
    pub user: Address,
    /// Underlying amount deposited.
    pub underlying_in: i128,
    /// PT minted (== YT minted at mint time).
    pub pt_out: i128,
    /// YT minted.
    pub yt_out: i128,
    /// Yield index at mint (the "ghost" snapshot the YT inherits).
    pub index_at_mint: i128,
}

/// `(TOPIC_REDEEM, user)` indexed; `data = RedeemEvent`.
///
/// `redeem_kind` distinguishes the two paths:
/// - `0`: pre-maturity redeem of PT+YT pair (one-for-one).
/// - `1`: post-maturity redeem of PT alone.
#[contracttype]
#[derive(Clone, Debug)]
pub struct RedeemEvent {
    /// User redeeming.
    pub user: Address,
    /// PT burned.
    pub pt_burned: i128,
    /// YT burned (zero on the post-maturity path).
    pub yt_burned: i128,
    /// Underlying returned.
    pub underlying_out: i128,
    /// `0` = pre-maturity pair redeem, `1` = post-maturity PT redeem.
    pub redeem_kind: u32,
}

/// `(TOPIC_YIELD, user)` indexed; `data = YieldClaimedEvent`.
#[contracttype]
#[derive(Clone, Debug)]
pub struct YieldClaimedEvent {
    /// YT holder claiming.
    pub user: Address,
    /// Underlying paid out as yield.
    pub yield_amount: i128,
    /// User's local yield index snapshot **before** the claim.
    pub user_index_before: i128,
    /// User's local yield index snapshot **after** the claim
    /// (== global index at claim time).
    pub user_index_after: i128,
}

/// `(TOPIC_INDEX,)` (single-tag, no indexed key); `data = IndexSyncEvent`.
///
/// Emitted whenever the global yield index is rolled forward. This
/// gives indexers a complete monotonic timeline of the index without
/// needing to reconstruct it from mint / claim flows.
#[contracttype]
#[derive(Clone, Debug)]
pub struct IndexSyncEvent {
    /// Global index after sync, in WAD.
    pub new_index: i128,
    /// Previous index, in WAD.
    pub old_index: i128,
    /// Ledger timestamp at sync (seconds since epoch).
    pub at_timestamp: u64,
}

/// `(TOPIC_SWAP, trader)` indexed; `data = SwapEvent`.
#[contracttype]
#[derive(Clone, Debug)]
pub struct SwapEvent {
    /// Trader.
    pub trader: Address,
    /// PT delta from the trader's perspective. Positive = trader
    /// received PT, negative = trader spent PT.
    pub pt_delta: i128,
    /// SY delta from the trader's perspective.
    pub sy_delta: i128,
    /// Fee paid in SY (already deducted from `sy_delta`).
    pub fee_sy: i128,
    /// Implied rate (WAD-scaled) **after** the swap.
    pub implied_rate_after: i128,
}

/// `(TOPIC_LIQ, provider)` indexed; `data = LiquidityEvent`.
///
/// `op` distinguishes:
/// - `0`: add (mint LP).
/// - `1`: remove (burn LP).
#[contracttype]
#[derive(Clone, Debug)]
pub struct LiquidityEvent {
    /// LP provider.
    pub provider: Address,
    /// PT contributed (positive on add, negative on remove).
    pub pt_delta: i128,
    /// SY contributed (positive on add, negative on remove).
    pub sy_delta: i128,
    /// LP tokens minted (`op=0`) or burned (`op=1`).
    pub lp_delta: i128,
    /// `0` = add, `1` = remove.
    pub op: u32,
}

/// `(TOPIC_OBS,)` (single-tag); `data = ObservationEvent`.
///
/// Emitted whenever the oracle ring buffer accepts a new observation.
#[contracttype]
#[derive(Clone, Debug)]
pub struct ObservationEvent {
    /// Implied rate captured, WAD-scaled.
    pub implied_rate: i128,
    /// Ledger timestamp of this observation (seconds since epoch).
    pub at_timestamp: u64,
    /// Index in the ring buffer that was written.
    pub slot: u32,
}

// -------------------------------------------------------------------------
// Helpers for constructing the indexed-topic Symbol from contracts.
// -------------------------------------------------------------------------

/// Convenience: build a fresh `Symbol` for a topic-tag string. Contracts
/// usually prefer `symbol_short!("mint")` directly; this helper exists
/// for the rare path that needs to dispatch on a runtime string.
#[must_use]
pub fn symbol(env: &soroban_sdk::Env, tag: &str) -> Symbol {
    Symbol::new(env, tag)
}
