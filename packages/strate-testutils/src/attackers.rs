//! Attacker harnesses.
//!
//! Each harness is a deterministic state machine that runs a known
//! attack end-to-end. The harnesses come in two flavours:
//!
//! 1. **Stub** (`cfg(not(feature = "wired"))`) — returns `Mitigated`
//!    immediately. This lets contract-internal unit tests compile while
//!    referencing the harness type.
//! 2. **Wired** (`cfg(feature = "wired")`) — drives real contract
//!    interactions against a [`crate::builders::Market`] populated by the
//!    wired builder, then computes the witness from on-chain balances.
//!
//! Each `run()` returns [`AttackResult::Mitigated`] when the protocol
//! defends successfully and [`AttackResult::Succeeded`] with the witness
//! amount when the protocol fails. Tests assert `Mitigated`; a
//! `Succeeded` return is itself the test failure.

use soroban_sdk::{Address, Env};

use crate::builders::Market;
use crate::AttackResult;

// =========================================================================
// C-01 — Donation inflation (first depositor)
// =========================================================================

pub struct DonationInflationAttacker {
    pub donation: i128,
    pub victim_mint: i128,
}

impl DonationInflationAttacker {
    #[must_use]
    pub fn standard() -> Self {
        Self {
            donation: 1_000_000 * crate::constants::ONE_UNDERLYING,
            victim_mint: 100 * crate::constants::ONE_UNDERLYING,
        }
    }

    /// Run the attack.
    #[cfg(not(feature = "wired"))]
    pub fn run(&self, _env: &Env, _market: &Market, _attacker: &Address) -> AttackResult {
        AttackResult::Mitigated
    }

    /// Run the attack against the real contracts.
    ///
    /// Sequence:
    /// 1. Attacker mints a small position (`min_mint = DEAD_SHARES + 1`
    ///    underlying — the YS rejects anything ≤ DEAD_SHARES on the
    ///    first deposit).
    /// 2. Attacker performs a direct underlying transfer (donation) to
    ///    the YS contract address — bypassing the mint path.
    /// 3. Victim attempts a normal `mint(victim_mint)`.
    /// 4. Witness: did victim's PT balance increment normally?
    #[cfg(feature = "wired")]
    pub fn run(&self, env: &Env, market: &Market, attacker: &Address) -> AttackResult {
        use soroban_sdk::token::{StellarAssetClient, TokenClient};

        let ys = yield_stripping::YieldStrippingClient::new(env, &market.yield_stripping);
        let pt = pt_token::PtTokenClient::new(env, &market.pt);
        let und = TokenClient::new(env, &market.underlying);
        let und_admin = StellarAssetClient::new(env, &market.underlying);

        // Fund the attacker with enough underlying for both moves.
        und_admin.mint(attacker, &(self.donation + 10_000_000));

        let start_attacker = und.balance(attacker);

        // 1. First deposit — must be > DEAD_SHARES (1000) to clear the
        // YS guard. We use 10 underlying.
        let small_mint = 10_000_000_i128; // 10 underlying in 7-decimal.
        let _ = ys.try_mint(attacker, attacker, &small_mint);

        // 2. Donation — direct transfer to YS.
        und.transfer(attacker, &market.yield_stripping, &self.donation);

        // 3. Victim mint.
        let victim = market.user(env, "victim");
        und_admin.mint(&victim, &self.victim_mint);
        let pre_victim_pt = pt.balance(&victim);
        let _ = ys.try_mint(&victim, &victim, &self.victim_mint);
        let post_victim_pt = pt.balance(&victim);
        let victim_received = post_victim_pt - pre_victim_pt;

        // Mitigation expectation: victim receives ~= victim_mint PT
        // (less DEAD_SHARES on the very-first-deposit path; we already
        // burned that on the attacker's mint, so victim is full).
        if victim_received <= 0 {
            // Attack succeeded — victim got dust. Compute attacker's
            // post-state.
            let end_attacker = und.balance(attacker);
            // Net of the donation: attacker spent `donation +
            // small_mint`; if they extract back more than the
            // small_mint they originally deposited via redeem/claim,
            // that's a successful exploit.
            let extracted = end_attacker - start_attacker;
            if extracted > 0 {
                return AttackResult::Succeeded {
                    extracted_underlying: extracted,
                };
            }
        }
        AttackResult::Mitigated
    }
}

// =========================================================================
// C-02 — Stale yield index at YT mint
// =========================================================================

pub struct StaleRateYtMintAttacker {
    pub rate_jump_rel_wad: i128,
    pub mint_underlying: i128,
}

impl StaleRateYtMintAttacker {
    #[must_use]
    pub fn standard() -> Self {
        Self {
            rate_jump_rel_wad: 5 * (crate::constants::WAD / 100),
            mint_underlying: 10_000 * crate::constants::ONE_UNDERLYING,
        }
    }

    #[cfg(not(feature = "wired"))]
    pub fn run(&self, _env: &Env, _market: &Market, _attacker: &Address) -> AttackResult {
        AttackResult::Mitigated
    }

    /// Sequence:
    /// 1. Sync the Oracle once to establish baseline.
    /// 2. Bump the mock Blend rate via the test-only `set_rate`.
    /// 3. Attacker mints YT.
    /// 4. Attacker calls `claim_yield` with no time elapsed since mint.
    /// 5. Witness: claim > 0 ⇒ attacker received stale-rate yield.
    #[cfg(feature = "wired")]
    pub fn run(&self, env: &Env, market: &Market, attacker: &Address) -> AttackResult {
        use soroban_sdk::token::{StellarAssetClient, TokenClient};

        let mock = crate::mock_blend::MockBlendPoolClient::new(env, &market.blend_pool);
        let ys = yield_stripping::YieldStrippingClient::new(env, &market.yield_stripping);
        let oracle = strate_oracle::OracleClient::new(env, &market.oracle);
        let und = TokenClient::new(env, &market.underlying);
        let und_admin = StellarAssetClient::new(env, &market.underlying);

        // 1. Establish baseline via oracle + YS sync.
        let _ = oracle.try_sync_rate();
        ys.sync_yield_index();

        let pre_balance = und.balance(attacker);
        und_admin.mint(attacker, &self.mint_underlying);

        // 2. Off-protocol rate jump. The Oracle's rate-limit is 5%, so
        // a 5% jump is the largest single-sample bump we can drive
        // before the oracle freezes. We deliberately use a
        // *sub-threshold* delta here so the rate-limit guard does not
        // mask the test — the C-02 mitigation must hold even on
        // legitimate fast rate moves.
        let r0 = mock.b_rate();
        let r1 = crate::mock_blend::bumped_rate(r0, self.rate_jump_rel_wad / 2);
        mock.set_rate(&r1);

        // 3. Mint YT. The YS mint path MUST sync the index before the
        // YT recipient's snapshot is taken.
        let _ = ys.try_mint(attacker, attacker, &self.mint_underlying);

        // 4. Immediate claim — zero time elapsed since mint.
        let claimed = ys.claim_yield(attacker);

        // 5. Witness.
        if claimed > 0 {
            let post_balance = und.balance(attacker);
            let extracted = post_balance - pre_balance;
            // We expect ~0 yield. A few wei is OK as rounding noise.
            if extracted > 10 {
                return AttackResult::Succeeded {
                    extracted_underlying: extracted,
                };
            }
        }
        AttackResult::Mitigated
    }
}

// =========================================================================
// C-03 — AMM math at maturity asymptote
// =========================================================================

pub struct NearMaturityDrainAttacker {
    pub seconds_before_maturity: u64,
    pub pt_in: i128,
    pub min_out: i128,
}

impl NearMaturityDrainAttacker {
    #[must_use]
    pub fn standard() -> Self {
        Self {
            seconds_before_maturity: 1,
            pt_in: 10 * crate::constants::ONE_UNDERLYING,
            min_out: 1,
        }
    }

    #[cfg(not(feature = "wired"))]
    pub fn run(&self, _env: &Env, _market: &Market, _attacker: &Address) -> AttackResult {
        AttackResult::Mitigated
    }

    /// Sequence:
    /// 1. Position ledger inside the lock window.
    /// 2. Attempt a swap.
    /// 3. Witness: revert ⇒ mitigated; success ⇒ failure.
    #[cfg(feature = "wired")]
    pub fn run(&self, env: &Env, market: &Market, attacker: &Address) -> AttackResult {
        let amm_addr = match &market.amm {
            Some(a) => a.clone(),
            None => return AttackResult::Mitigated, // No AMM => no attack surface.
        };
        let amm = amm::AmmContractClient::new(env, &amm_addr);

        let target_ts = market.maturity.saturating_sub(self.seconds_before_maturity);
        crate::time::advance_to(env, target_ts);

        // try_swap_exact_pt_for_sy returns Err on revert.
        let result = amm.try_swap_exact_pt_for_sy(attacker, &self.pt_in, &self.min_out);
        match result {
            Err(_) => AttackResult::Mitigated, // lock window fired
            Ok(Err(_)) => AttackResult::Mitigated,
            Ok(Ok(out)) => {
                if out > 0 {
                    AttackResult::Succeeded {
                        extracted_underlying: out,
                    }
                } else {
                    AttackResult::Mitigated
                }
            }
        }
    }
}

// =========================================================================
// H-01 — YT transfer recipient stale snapshot
// =========================================================================

pub struct YtTransferStaleSnapshotAttacker {
    pub sender_mint: i128,
    pub transfer_bps: u32,
    pub pre_transfer_seconds: u64,
    pub post_transfer_seconds: u64,
}

impl YtTransferStaleSnapshotAttacker {
    #[must_use]
    pub fn standard() -> Self {
        Self {
            sender_mint: 1_000 * crate::constants::ONE_UNDERLYING,
            transfer_bps: 5_000,
            pre_transfer_seconds: 7 * crate::time::DAY,
            post_transfer_seconds: 7 * crate::time::DAY,
        }
    }

    #[cfg(not(feature = "wired"))]
    pub fn run(&self, _env: &Env, _market: &Market, _b: &Address) -> AttackResult {
        AttackResult::Mitigated
    }

    /// Sequence:
    /// 1. Alice mints, capturing fresh snapshot.
    /// 2. Time + rate advance.
    /// 3. Alice transfers half her YT to Bob.
    /// 4. More time + rate advance.
    /// 5. Bob claims.
    /// 6. Witness: Bob's claim > fair (post-transfer-only) share.
    #[cfg(feature = "wired")]
    pub fn run(&self, env: &Env, market: &Market, bob: &Address) -> AttackResult {
        use soroban_sdk::token::{StellarAssetClient, TokenClient};

        let alice = market.user(env, "alice");
        let ys = yield_stripping::YieldStrippingClient::new(env, &market.yield_stripping);
        let yt = yt_token::YtTokenClient::new(env, &market.yt);
        let mock = crate::mock_blend::MockBlendPoolClient::new(env, &market.blend_pool);
        let und = TokenClient::new(env, &market.underlying);
        let und_admin = StellarAssetClient::new(env, &market.underlying);

        // 1.
        und_admin.mint(&alice, &self.sender_mint);
        let _ = ys.try_mint(&alice, &alice, &self.sender_mint);

        // 2.
        crate::time::advance_by(env, self.pre_transfer_seconds);
        mock.set_rate(&crate::mock_blend::bumped_rate(
            mock.b_rate(),
            crate::constants::WAD / 100, // +1%
        ));

        // 3.
        let bal = yt.balance(&alice);
        let xfer = bal * i128::from(self.transfer_bps) / 10_000;
        if xfer > 0 {
            yt.transfer(&alice, bob, &xfer);
        }

        // 4.
        crate::time::advance_by(env, self.post_transfer_seconds);
        mock.set_rate(&crate::mock_blend::bumped_rate(
            mock.b_rate(),
            crate::constants::WAD / 100,
        ));

        // 5.
        let pre = und.balance(bob);
        let _ = ys.try_claim_yield(bob);
        let post = und.balance(bob);
        let claimed = post - pre;

        // 6. Fair share is bounded above by `xfer * delta_index_post`.
        // Since the YT_update hook settles Bob *before* the transfer
        // credits him with anything, Bob's pre-transfer accrual is
        // owed to Alice (it's already in her AccruedYield pot at the
        // moment of transfer). Bob's claim therefore reflects only
        // the post-transfer accrual.
        //
        // Conservative bound: claimed should be small relative to the
        // total yield accrued in the test window. We compute a fair
        // upper bound: `xfer * (idx_now - idx_at_transfer) / WAD`. We
        // don't have idx_at_transfer (transfer didn't expose it), so
        // we use the loose bound that claimed must not exceed Alice's
        // initial principal (sender_mint).
        if claimed > self.sender_mint / 10 {
            // 10% of principal in 14 days is well above even
            // 100%-APR — anything that large is a clear exploit.
            return AttackResult::Succeeded {
                extracted_underlying: claimed,
            };
        }
        AttackResult::Mitigated
    }
}

// =========================================================================
// Sanity tests for the harness scaffolding itself.
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_configs_have_positive_inputs() {
        let d = DonationInflationAttacker::standard();
        assert!(d.donation > 0 && d.victim_mint > 0);

        let s = StaleRateYtMintAttacker::standard();
        assert!(s.rate_jump_rel_wad > 0 && s.mint_underlying > 0);

        let n = NearMaturityDrainAttacker::standard();
        assert!(n.seconds_before_maturity > 0 && n.pt_in > 0);

        let y = YtTransferStaleSnapshotAttacker::standard();
        assert!(y.sender_mint > 0);
        assert!(y.transfer_bps > 0 && y.transfer_bps <= 10_000);
    }

    #[test]
    fn attack_result_is_mitigated_distinguishes() {
        let m = AttackResult::Mitigated;
        let s = AttackResult::Succeeded {
            extracted_underlying: 42,
        };
        assert!(m.is_mitigated());
        assert!(!s.is_mitigated());
        assert_eq!(m.extracted(), 0);
        assert_eq!(s.extracted(), 42);
    }
}
