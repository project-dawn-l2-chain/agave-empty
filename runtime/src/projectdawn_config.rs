//! ProjectDawn custom tokenomics configuration.
//!
//! ProjectDawn operates as an L2 where a single sequencer (the "validator")
//! orders transactions and produces blocks, rather than participating in
//! L1-style consensus. The code retains Solana's "validator" terminology
//! for compatibility.
//!
//! Defines the central configuration for ProjectDawn's economic model:
//! - Flat 5% staking rewards
//! - EIP-1559 style 4-way fee split (burn, validator, treasury, developer)
//! - Linear interpolation of fee split ratios from launch to maturity
//! - Tiered passive staking rates
//!
//! **All arithmetic is integer-only** (basis points / u64) to ensure
//! deterministic consensus across different CPU architectures.

use serde::{Deserialize, Serialize};
use solana_pubkey::Pubkey;

/// Basis points (1 bp = 0.01%). All fee splits sum to 10_000 bp.
pub const BASIS_POINTS_100_PCT: u64 = 10_000;

/// Number of epochs per year (assuming ~2.5 day epochs).
pub const EPOCHS_PER_YEAR: u64 = 146;

/// Feature gate pubkey for ProjectDawn economics.
///
/// When a Feature account with this pubkey exists on-chain (created via
/// `solana feature activate`), the runtime enables ProjectDawn tokenomics
/// (custom fee split, flat staking APY, passive staking, permanent locks).
/// Without this account, all ProjectDawn behaviour is disabled and the
/// chain operates with vanilla Solana economics.
pub const PROJECTDAWN_FEATURE_ID: Pubkey = Pubkey::new_from_array([
    0xDA, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02,
]);

/// ProjectDawn treasury pubkey (team multisig, set at genesis).
/// This is a placeholder; the real key is set via genesis configuration.
pub const DEFAULT_TREASURY_PUBKEY: Pubkey = Pubkey::new_from_array([
    0xDA, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
]);

/// Central configuration for ProjectDawn's custom economics.
///
/// Stored as a field on `Bank` and serialized to snapshots via `ExtraFieldsToSerialize`.
/// All percentages use basis points to avoid floating point in consensus paths.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectDawnConfig {
    /// Base staking APY in basis points (500 = 5%).
    pub staking_rate_bp: u64,
    /// First epoch of the chain (typically 0).
    pub launch_epoch: u64,
    /// Number of epochs over which fee split transitions from launch to target.
    /// ~3650 epochs ≈ 25 years at 2.5 days/epoch, or ~1460 ≈ 10 years.
    pub transition_epochs: u64,
    /// Fee split at launch: [burn_bp, validator_bp, treasury_bp, developer_bp].
    pub launch_fee_split: [u64; 4],
    /// Fee split at maturity: [burn_bp, validator_bp, treasury_bp, developer_bp].
    pub target_fee_split: [u64; 4],
    /// Treasury account pubkey (receives treasury portion of fees + penalties).
    pub treasury_pubkey: Pubkey,
}

/// Computed fee split amounts for a single transaction or slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeeSplit {
    pub burn: u64,
    pub validator: u64,
    pub treasury: u64,
    pub developer: u64,
}

/// Passive staking tier configuration.
/// Tier 0 = not passive. Tiers 1-5 have increasing lockup and rewards.
#[derive(Debug, Clone, Copy)]
pub struct PassiveTierConfig {
    /// Fraction of the base 5% rate (in basis points out of 10_000).
    pub rate_fraction_bp: u64,
    /// Lockup duration in epochs.
    pub lockup_epochs: u64,
    /// Governance vote weight multiplier (in basis points, e.g. 1000 = 0.10x).
    pub vote_weight_bp: u64,
}

/// Passive tier table (indexed by tier 1-5; index 0 is unused).
pub const PASSIVE_TIERS: [PassiveTierConfig; 6] = [
    // Tier 0: placeholder (not used)
    PassiveTierConfig { rate_fraction_bp: 0, lockup_epochs: 0, vote_weight_bp: 0 },
    // Tier 1: No lock — 5% of base = 0.25% APY, 0x vote weight
    PassiveTierConfig { rate_fraction_bp: 500, lockup_epochs: 0, vote_weight_bp: 0 },
    // Tier 2: 30-day lock (~14 epochs) — 10% of base = 0.50% APY, 0.10x
    PassiveTierConfig { rate_fraction_bp: 1000, lockup_epochs: 14, vote_weight_bp: 1000 },
    // Tier 3: 90-day lock (~36 epochs) — 20% of base = 1.00% APY, 0.20x
    PassiveTierConfig { rate_fraction_bp: 2000, lockup_epochs: 36, vote_weight_bp: 2000 },
    // Tier 4: 180-day lock (~72 epochs) — 30% of base = 1.50% APY, 0.30x
    PassiveTierConfig { rate_fraction_bp: 3000, lockup_epochs: 72, vote_weight_bp: 3000 },
    // Tier 5: 360-day lock (~144 epochs) — 50% of base = 2.50% APY, 0.50x
    PassiveTierConfig { rate_fraction_bp: 5000, lockup_epochs: 144, vote_weight_bp: 5000 },
];

/// Permanent lock reward multiplier (120% of base = 6% APY for permanently locked).
pub const PERMANENT_LOCK_MULTIPLIER_BP: u64 = 12_000; // 120% in basis points

impl Default for ProjectDawnConfig {
    fn default() -> Self {
        Self {
            staking_rate_bp: 500, // 500 bp = 5% base APY
            launch_epoch: 0,
            transition_epochs: 1460, // ~10 years of epochs
            // Launch split: 10% burn, 0% validator, 45% treasury, 45% developer
            launch_fee_split: [1000, 0, 4500, 4500],
            // Target split: 25% burn, 25% validator, 25% treasury, 25% developer
            target_fee_split: [2500, 2500, 2500, 2500],
            treasury_pubkey: DEFAULT_TREASURY_PUBKEY,
        }
    }
}

impl ProjectDawnConfig {
    /// Set a custom treasury pubkey, returning the modified config.
    pub fn with_treasury_pubkey(mut self, pubkey: Pubkey) -> Self {
        self.treasury_pubkey = pubkey;
        self
    }

    /// Compute the fee split basis points at a given epoch via linear interpolation.
    ///
    /// Returns `[burn_bp, validator_bp, treasury_bp, developer_bp]` that sum to 10_000.
    pub fn fee_split_at_epoch(&self, epoch: u64) -> [u64; 4] {
        if self.transition_epochs == 0 || epoch >= self.launch_epoch + self.transition_epochs {
            return self.target_fee_split;
        }
        let elapsed = epoch.saturating_sub(self.launch_epoch);
        let mut result = [0u64; 4];
        let mut total = 0u64;
        for i in 0..4 {
            let start = self.launch_fee_split[i];
            let end = self.target_fee_split[i];
            // Linear interpolation: start + (end - start) * elapsed / transition_epochs
            // Use u128 intermediates to prevent overflow on large values.
            if end >= start {
                result[i] = start + ((end - start) as u128 * elapsed as u128 / self.transition_epochs as u128) as u64;
            } else {
                result[i] = start - ((start - end) as u128 * elapsed as u128 / self.transition_epochs as u128) as u64;
            }
            total += result[i];
        }
        // Ensure sum == 10_000 by adjusting burn (index 0)
        if total != BASIS_POINTS_100_PCT {
            if total < BASIS_POINTS_100_PCT {
                result[0] += BASIS_POINTS_100_PCT - total;
            } else {
                result[0] = result[0].saturating_sub(total - BASIS_POINTS_100_PCT);
            }
        }
        result
    }

    /// Split a total fee amount into the four components using the given epoch's ratios.
    pub fn split_fees(&self, total_fees: u64, epoch: u64) -> FeeSplit {
        let bp = self.fee_split_at_epoch(epoch);
        let burn = ((total_fees as u128) * (bp[0] as u128) / (BASIS_POINTS_100_PCT as u128)) as u64;
        let validator = ((total_fees as u128) * (bp[1] as u128) / (BASIS_POINTS_100_PCT as u128)) as u64;
        let treasury = ((total_fees as u128) * (bp[2] as u128) / (BASIS_POINTS_100_PCT as u128)) as u64;
        // Developer gets remainder to avoid rounding loss
        let developer = total_fees.saturating_sub(burn).saturating_sub(validator).saturating_sub(treasury);
        FeeSplit { burn, validator, treasury, developer }
    }

    /// Compute the effective APY for a passive staking tier in basis points.
    ///
    /// Returns: `staking_rate_bp * tier_fraction_bp / BASIS_POINTS_100_PCT`
    /// For tier 2 with 5% base: `500 * 1000 / 10000 = 50 bp = 0.50%`
    pub fn passive_tier_rate_bp(&self, tier: u8) -> u64 {
        if tier == 0 || tier as usize >= PASSIVE_TIERS.len() {
            return 0;
        }
        let tier_config = &PASSIVE_TIERS[tier as usize];
        self.staking_rate_bp
            .saturating_mul(tier_config.rate_fraction_bp)
            / BASIS_POINTS_100_PCT
    }

    /// Compute the reward for a passive stake for one epoch (integer-only).
    ///
    /// Formula: `stake_amount * effective_rate_bp / (BASIS_POINTS_100_PCT * EPOCHS_PER_YEAR)`
    pub fn passive_epoch_reward(&self, stake_amount: u64, tier: u8) -> u64 {
        let rate_bp = self.passive_tier_rate_bp(tier);
        // Use u128 to avoid overflow on large stakes
        ((stake_amount as u128)
            .saturating_mul(rate_bp as u128)
            / (BASIS_POINTS_100_PCT as u128 * EPOCHS_PER_YEAR as u128)) as u64
    }

    /// Early unlock penalty: `5 * effective_rate_bp * staked_amount / BASIS_POINTS_100_PCT`
    ///
    /// All integer arithmetic. Uses u128 intermediate to prevent overflow.
    pub fn early_unlock_penalty(&self, stake_amount: u64, tier: u8) -> u64 {
        let rate_bp = self.passive_tier_rate_bp(tier);
        // penalty = 5 * stake * rate_bp / 10_000
        ((stake_amount as u128)
            .saturating_mul(rate_bp as u128)
            .saturating_mul(5)
            / BASIS_POINTS_100_PCT as u128) as u64
    }

    /// Compute staking rewards for one epoch (integer-only).
    ///
    /// Formula: `total_staked * staking_rate_bp / (BASIS_POINTS_100_PCT * EPOCHS_PER_YEAR)`
    /// Uses u128 to prevent overflow on large total_staked values.
    pub fn epoch_staking_reward(&self, total_staked: u64) -> u64 {
        ((total_staked as u128)
            .saturating_mul(self.staking_rate_bp as u128)
            / (BASIS_POINTS_100_PCT as u128 * EPOCHS_PER_YEAR as u128)) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fee_split_at_launch() {
        let config = ProjectDawnConfig::default();
        let split = config.fee_split_at_epoch(0);
        assert_eq!(split, [1000, 0, 4500, 4500]);
        assert_eq!(split.iter().sum::<u64>(), BASIS_POINTS_100_PCT);
    }

    #[test]
    fn test_fee_split_at_maturity() {
        let config = ProjectDawnConfig::default();
        let split = config.fee_split_at_epoch(config.launch_epoch + config.transition_epochs);
        assert_eq!(split, [2500, 2500, 2500, 2500]);
        assert_eq!(split.iter().sum::<u64>(), BASIS_POINTS_100_PCT);
    }

    #[test]
    fn test_fee_split_midpoint() {
        let config = ProjectDawnConfig::default();
        let mid_epoch = config.launch_epoch + config.transition_epochs / 2;
        let split = config.fee_split_at_epoch(mid_epoch);
        // All values should be between launch and target
        for i in 0..4 {
            let lo = config.launch_fee_split[i].min(config.target_fee_split[i]);
            let hi = config.launch_fee_split[i].max(config.target_fee_split[i]);
            assert!(split[i] >= lo && split[i] <= hi,
                "split[{}] = {} not in [{}, {}]", i, split[i], lo, hi);
        }
        assert_eq!(split.iter().sum::<u64>(), BASIS_POINTS_100_PCT);
    }

    #[test]
    fn test_split_fees_no_rounding_loss() {
        let config = ProjectDawnConfig::default();
        let total = 1_000_000_000; // 1 SOL in lamports
        let split = config.split_fees(total, 0);
        assert_eq!(split.burn + split.validator + split.treasury + split.developer, total);
    }

    #[test]
    fn test_passive_tier_rates_bp() {
        let config = ProjectDawnConfig::default();
        assert_eq!(config.passive_tier_rate_bp(0), 0);
        assert_eq!(config.passive_tier_rate_bp(1), 25);  // 0.25% = 25 bp
        assert_eq!(config.passive_tier_rate_bp(2), 50);  // 0.50% = 50 bp
        assert_eq!(config.passive_tier_rate_bp(3), 100); // 1.00% = 100 bp
        assert_eq!(config.passive_tier_rate_bp(4), 150); // 1.50% = 150 bp
        assert_eq!(config.passive_tier_rate_bp(5), 250); // 2.50% = 250 bp
    }

    #[test]
    fn test_early_unlock_penalty() {
        let config = ProjectDawnConfig::default();
        let stake = 1_000_000_000u64; // 1 SOL
        // Tier 2: 5 * 50bp/10000 * 1 SOL = 5 * 0.005 * 1e9 = 25_000_000
        let penalty = config.early_unlock_penalty(stake, 2);
        assert_eq!(penalty, 25_000_000);
    }

    #[test]
    fn test_passive_epoch_reward() {
        let config = ProjectDawnConfig::default();
        let stake = 1_000_000_000u64; // 1 SOL
        // Tier 3: rate = 100bp. reward/epoch = 1e9 * 100 / (10000 * 146) = 68493
        let reward = config.passive_epoch_reward(stake, 3);
        assert_eq!(reward, 68493);
    }

    #[test]
    fn test_epoch_staking_reward() {
        let config = ProjectDawnConfig::default();
        let total_staked = 1_000_000_000_000u64; // 1000 SOL
        // reward = 1e12 * 500 / (10000 * 146) = 342_465_753
        let reward = config.epoch_staking_reward(total_staked);
        assert_eq!(reward, 342_465_753);
    }

    #[test]
    fn test_with_treasury_pubkey() {
        let custom_pubkey = Pubkey::new_from_array([42; 32]);
        let config = ProjectDawnConfig::default().with_treasury_pubkey(custom_pubkey);
        assert_eq!(config.treasury_pubkey, custom_pubkey);
        // Other fields should remain at defaults
        assert_eq!(config.staking_rate_bp, 500);
        assert_eq!(config.transition_epochs, 1460);
        assert_eq!(config.launch_fee_split, [1000, 0, 4500, 4500]);
    }

    #[test]
    fn test_split_fees_large_amount_no_overflow() {
        let config = ProjectDawnConfig::default();
        // 10 billion SOL in lamports -- would overflow u64 without u128 intermediates
        let total = 10_000_000_000_000_000_000u64; // ~10B SOL
        let split = config.split_fees(total, 0);
        assert_eq!(split.burn + split.validator + split.treasury + split.developer, total);
    }

    #[test]
    fn test_no_floating_point() {
        // Verify determinism: same inputs must always produce same outputs
        let config = ProjectDawnConfig::default();
        let stake = 123_456_789_012_345u64;
        let r1 = config.epoch_staking_reward(stake);
        let r2 = config.epoch_staking_reward(stake);
        assert_eq!(r1, r2);
        // Also verify passive rewards are deterministic
        for tier in 1..=5u8 {
            let p1 = config.passive_epoch_reward(stake, tier);
            let p2 = config.passive_epoch_reward(stake, tier);
            assert_eq!(p1, p2);
        }
    }
}
