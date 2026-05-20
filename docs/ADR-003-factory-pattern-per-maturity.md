# ADR-003: Factory Pattern Per Maturity

## Status
Accepted — 2026-05-19

## Context
A PT/YT pair represents a fixed claim on yield up to a specific maturity timestamp. A PT for Dec 31 2026 and a PT for Mar 31 2027 are economically distinct assets with different durations, prices, and AMM curves. Trying to fold multiple maturities into a single contract via parameterized mappings would (a) make `maturity` mutable per call rather than immutable per market, (b) collapse the AMM's `scalar_root` into shared state, (c) prevent per-market pause, and (d) make storage TTL accounting impossibly tangled.

Strate therefore deploys a fully separate market — a fresh `{YS, PT, YT, AMM, Oracle}` quintuple — for every (underlying, maturity) pair. The Factory is the single source of truth for which markets exist.

## Decision

### Factory responsibilities
1. **Deploy** a complete market on demand via `deploy_market(underlying, maturity, scalar_root, oracle_lookups)`.
2. **Register** the deployed quintuple in a queryable on-chain index.
3. **Authorize** deployments — permissioned in v1, permissionless in v2 (see Permissions below).
4. **Gate** itself with `paused` to halt new deployments during incidents (does not affect existing markets).

The Factory does **not**:
- Hold user funds.
- Mediate cross-market interactions (markets are independent).
- Have any setter on a deployed market (immutability per ADR-002).

### Deployment API

```
fn deploy_market(
  underlying: Address,
  maturity: u64,        // UNIX seconds, must be > now + MIN_DURATION (e.g., 7 days)
  scalar_root: i128,    // AMM curve param
  oracle_config: OracleConfig,
) -> MarketAddresses { ys, pt, yt, amm, oracle }
```

Validation in the Factory:
- `maturity > now + 7 days` and `maturity < now + 2 years` (bounded horizons; long-dated math in ADR-004).
- `underlying` is whitelisted (v1: bUSDC only; whitelist expands via timelocked admin).
- `scalar_root` falls in `[SCALAR_MIN, SCALAR_MAX]` to prevent obviously broken curves.
- No existing market at the derived address (re-deploy attempt fails cleanly).

### Address derivation (deterministic via salt)
Soroban supports deterministic deployment from a WASM hash + salt + deployer. We use:

```
salt = sha256(
  b"strate.v1" ||
  underlying.to_bytes() ||
  maturity.to_be_bytes() ||
  contract_kind_tag   // one of "ys" / "pt" / "yt" / "amm" / "oracle"
)
```

Five salts, one per contract in the market. The derived addresses are predictable off-chain before deployment, which enables:
- Front-ends to compute expected addresses and verify the on-chain registry matches.
- Auditors to verify "the PT for bUSDC@2026-12-31 lives at exactly this address."
- Block explorers and indexers to pre-register markets.

Note: we use `sha256` rather than `keccak256` because it's the Stellar/Soroban-native hash and avoids importing a Keccak dependency. Functionally equivalent for salt purposes.

### Constructor wiring
The Factory deploys the five contracts in dependency order, then wires them in a single transaction:

```
1. oracle  = deploy(Oracle WASM, salt_oracle, args=[underlying, oracle_config])
2. pt      = deploy(PT WASM,     salt_pt,     args=[<placeholder>])
3. yt      = deploy(YT WASM,     salt_yt,     args=[<placeholder>])
4. amm     = deploy(AMM WASM,    salt_amm,    args=[pt, yt, oracle, maturity, scalar_root])
5. ys      = deploy(YS WASM,     salt_ys,     args=[underlying, pt, yt, oracle, amm, maturity])
6. pt.initialize_minter(ys)      # one-shot, irreversible
7. yt.initialize_minter(ys)      # one-shot, irreversible
```

PT and YT use a one-shot `initialize_minter` instead of taking `ys` in the constructor because `ys`'s address depends on PT/YT addresses being known first. The one-shot init reverts on second call, preserving immutability after wiring.

### Registry storage
The Factory keeps a persistent `Map<MarketKey, MarketAddresses>` where `MarketKey = (underlying, maturity)`. Reads:

- `get_market(underlying, maturity) -> Option<MarketAddresses>`
- `list_markets(underlying, offset, limit) -> Vec<MarketAddresses>`
- `market_count() -> u64`

The registry is append-only — markets cannot be removed. A bad market can be `paused` by its admin, and the Factory can mark it `deprecated: bool` for UI hints, but it stays on-chain forever for historical PT redemption.

### Permissions

| Phase | Who can call `deploy_market` |
|---|---|
| v1 (mainnet Q3 2026) | Strate Labs admin only |
| v2 (post-audit confidence) | Permissionless, with deposit |

V1 is admin-gated because a misconfigured `scalar_root` or off-by-one `maturity` produces a real market with real loss potential, and we can't expect retail users to validate it. V2 transitions to permissionless once tooling exists for users to verify market parameters pre-trade (likely as a UI gate, then on-chain via a `validate_market_params()` view). Permissionless v2 will require a small XLM deposit refunded after maturity to prevent spam markets.

## Consequences
- **Pro:** Every market is a clean room — no shared mutable state, no cross-maturity contagion.
- **Pro:** Deterministic addresses give integrators and auditors a strong verification primitive.
- **Pro:** Append-only registry preserves redemption rights for matured markets indefinitely.
- **Con:** 5 contracts per maturity means deployment gas is non-trivial. Acceptable on Soroban; amortized over market lifetime.
- **Con:** Liquidity fragments across maturities. Mitigated by AMM aggregator UIs and routing layers — not a protocol-layer concern.

## Alternatives Considered
1. **Single multi-maturity YS contract.** Rejected — collapses maturity from immutable to per-call, defeats ADR-002, and makes per-market pause impossible.
2. **CREATE2-equivalent without registry.** Rejected — no on-chain enumeration; integrators would scrape events forever. Registry cost is negligible vs. UX gain.
3. **Permissionless from v1.** Rejected — premature. The cost of one bad-parameter market on launch reputation outweighs the openness benefit.
4. **Factory-less; markets deployed by admin scripts.** Rejected — no on-chain provenance, no verifiable address derivation.
