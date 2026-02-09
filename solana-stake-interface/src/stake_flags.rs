#[cfg(feature = "borsh")]
use borsh::{BorshDeserialize, BorshSchema, BorshSerialize};

/// Additional flags for stake state.
#[cfg_attr(feature = "frozen-abi", derive(solana_frozen_abi_macro::AbiExample))]
#[cfg_attr(
    feature = "borsh",
    derive(BorshSerialize, BorshDeserialize, BorshSchema),
    borsh(crate = "borsh")
)]
#[cfg_attr(
    feature = "serde",
    derive(serde_derive::Deserialize, serde_derive::Serialize)
)]
#[derive(Copy, PartialEq, Eq, Clone, PartialOrd, Ord, Hash, Debug)]
pub struct StakeFlags {
    bits: u8,
}

#[cfg(feature = "borsh")]
impl borsh0_10::de::BorshDeserialize for StakeFlags {
    fn deserialize_reader<R: borsh0_10::maybestd::io::Read>(
        reader: &mut R,
    ) -> ::core::result::Result<Self, borsh0_10::maybestd::io::Error> {
        Ok(Self {
            bits: borsh0_10::BorshDeserialize::deserialize_reader(reader)?,
        })
    }
}

#[cfg(feature = "borsh")]
impl borsh0_10::BorshSchema for StakeFlags {
    fn declaration() -> borsh0_10::schema::Declaration {
        "StakeFlags".to_string()
    }
    fn add_definitions_recursively(
        definitions: &mut borsh0_10::maybestd::collections::HashMap<
            borsh0_10::schema::Declaration,
            borsh0_10::schema::Definition,
        >,
    ) {
        let fields = borsh0_10::schema::Fields::NamedFields(<[_]>::into_vec(
            borsh0_10::maybestd::boxed::Box::new([(
                "bits".to_string(),
                <u8 as borsh0_10::BorshSchema>::declaration(),
            )]),
        ));
        let definition = borsh0_10::schema::Definition::Struct { fields };
        Self::add_definition(
            <Self as borsh0_10::BorshSchema>::declaration(),
            definition,
            definitions,
        );
        <u8 as borsh0_10::BorshSchema>::add_definitions_recursively(definitions);
    }
}

#[cfg(feature = "borsh")]
impl borsh0_10::ser::BorshSerialize for StakeFlags {
    fn serialize<W: borsh0_10::maybestd::io::Write>(
        &self,
        writer: &mut W,
    ) -> ::core::result::Result<(), borsh0_10::maybestd::io::Error> {
        borsh0_10::BorshSerialize::serialize(&self.bits, writer)?;
        Ok(())
    }
}

/// Bit layout:
///   bit 0: MUST_FULLY_ACTIVATE_BEFORE_DEACTIVATION_IS_PERMITTED (deprecated)
///   bit 1: PERMANENTLY_LOCKED — principal can never be withdrawn
///   bits 2-3: reserved
///   bits 4-6: passive staking tier (0 = not passive, 1-5 = tier)
///   bit 7: reserved
impl StakeFlags {
    ///  Stake must be fully activated before deactivation is allowed (bit 0).
    #[deprecated(
        since = "2.1.0",
        note = "This flag will be removed because it was only used for `redelegate`, which will not be enabled."
    )]
    pub const MUST_FULLY_ACTIVATE_BEFORE_DEACTIVATION_IS_PERMITTED: Self =
        Self { bits: 0b0000_0001 };

    /// Permanently locked stake — principal cannot be withdrawn, only rewards (bit 1).
    pub const PERMANENTLY_LOCKED: Self = Self { bits: 0b0000_0010 };

    pub const fn empty() -> Self {
        Self { bits: 0 }
    }

    pub const fn contains(&self, other: Self) -> bool {
        (self.bits & other.bits) == other.bits
    }

    pub fn remove(&mut self, other: Self) {
        self.bits &= !other.bits;
    }

    pub fn set(&mut self, other: Self) {
        self.bits |= other.bits;
    }

    pub const fn union(self, other: Self) -> Self {
        Self {
            bits: self.bits | other.bits,
        }
    }

    /// Returns the passive staking tier encoded in bits 4-6 (0 = not passive, 1-5 = tier).
    pub const fn passive_tier(&self) -> u8 {
        (self.bits >> 4) & 0x07
    }

    /// Sets the passive staking tier in bits 4-6. Valid tiers are 0-5.
    /// Out-of-range values (> 5) are clamped to 0 (not passive).
    pub fn set_passive_tier(&mut self, tier: u8) {
        let clamped = if tier > 5 { 0 } else { tier };
        self.bits = (self.bits & 0x8F) | ((clamped & 0x07) << 4);
    }

    /// Returns the raw bits value.
    pub const fn bits(&self) -> u8 {
        self.bits
    }
}

impl Default for StakeFlags {
    fn default() -> Self {
        StakeFlags::empty()
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    #[allow(deprecated)]
    fn test_stake_flags() {
        let mut f = StakeFlags::empty();
        assert!(!f.contains(StakeFlags::MUST_FULLY_ACTIVATE_BEFORE_DEACTIVATION_IS_PERMITTED));

        f.set(StakeFlags::MUST_FULLY_ACTIVATE_BEFORE_DEACTIVATION_IS_PERMITTED);
        assert!(f.contains(StakeFlags::MUST_FULLY_ACTIVATE_BEFORE_DEACTIVATION_IS_PERMITTED));

        f.remove(StakeFlags::MUST_FULLY_ACTIVATE_BEFORE_DEACTIVATION_IS_PERMITTED);
        assert!(!f.contains(StakeFlags::MUST_FULLY_ACTIVATE_BEFORE_DEACTIVATION_IS_PERMITTED));

        let f1 = StakeFlags::empty();
        let f2 = StakeFlags::empty();
        let f3 = f1.union(f2);
        assert!(!f3.contains(StakeFlags::MUST_FULLY_ACTIVATE_BEFORE_DEACTIVATION_IS_PERMITTED));

        let f1 = StakeFlags::MUST_FULLY_ACTIVATE_BEFORE_DEACTIVATION_IS_PERMITTED;
        let f2 = StakeFlags::empty();
        let f3 = f1.union(f2);
        assert!(f3.contains(StakeFlags::MUST_FULLY_ACTIVATE_BEFORE_DEACTIVATION_IS_PERMITTED));

        let f1 = StakeFlags::empty();
        let f2 = StakeFlags::MUST_FULLY_ACTIVATE_BEFORE_DEACTIVATION_IS_PERMITTED;
        let f3 = f1.union(f2);
        assert!(f3.contains(StakeFlags::MUST_FULLY_ACTIVATE_BEFORE_DEACTIVATION_IS_PERMITTED));

        let f1 = StakeFlags::MUST_FULLY_ACTIVATE_BEFORE_DEACTIVATION_IS_PERMITTED;
        let f2 = StakeFlags::MUST_FULLY_ACTIVATE_BEFORE_DEACTIVATION_IS_PERMITTED;
        let f3 = f1.union(f2);
        assert!(f3.contains(StakeFlags::MUST_FULLY_ACTIVATE_BEFORE_DEACTIVATION_IS_PERMITTED));
    }

    #[test]
    fn test_permanently_locked_flag() {
        // Set and contains
        let mut f = StakeFlags::empty();
        assert!(!f.contains(StakeFlags::PERMANENTLY_LOCKED));
        f.set(StakeFlags::PERMANENTLY_LOCKED);
        assert!(f.contains(StakeFlags::PERMANENTLY_LOCKED));

        // Remove
        f.remove(StakeFlags::PERMANENTLY_LOCKED);
        assert!(!f.contains(StakeFlags::PERMANENTLY_LOCKED));

        // Union with empty
        let f1 = StakeFlags::PERMANENTLY_LOCKED;
        let f2 = StakeFlags::empty();
        let f3 = f1.union(f2);
        assert!(f3.contains(StakeFlags::PERMANENTLY_LOCKED));

        // Union symmetric
        let f3 = f2.union(f1);
        assert!(f3.contains(StakeFlags::PERMANENTLY_LOCKED));

        // Union with itself
        let f3 = f1.union(f1);
        assert!(f3.contains(StakeFlags::PERMANENTLY_LOCKED));

        // Verify the raw bit value
        assert_eq!(StakeFlags::PERMANENTLY_LOCKED.bits(), 0b0000_0010);
    }

    #[test]
    fn test_passive_tier_set_get() {
        for tier in 1..=5u8 {
            let mut f = StakeFlags::empty();
            f.set_passive_tier(tier);
            assert_eq!(
                f.passive_tier(),
                tier,
                "set_passive_tier({tier}) should round-trip"
            );
        }
    }

    #[test]
    fn test_passive_tier_zero() {
        let mut f = StakeFlags::empty();
        f.set_passive_tier(0);
        assert_eq!(f.passive_tier(), 0, "tier 0 means not passive");

        // Also verify clearing a previously-set tier
        f.set_passive_tier(3);
        assert_eq!(f.passive_tier(), 3);
        f.set_passive_tier(0);
        assert_eq!(f.passive_tier(), 0, "clearing tier back to 0 should work");
    }

    #[test]
    fn test_passive_tier_out_of_range() {
        // Tiers 6 and 7 are out-of-spec (only 0-5 valid); bounds clamping resets to 0
        let mut f = StakeFlags::empty();
        f.set_passive_tier(6);
        assert_eq!(f.passive_tier(), 0, "tier 6 should be clamped to 0");

        f.set_passive_tier(7);
        assert_eq!(f.passive_tier(), 0, "tier 7 should be clamped to 0");
    }

    #[test]
    fn test_passive_tier_large_input() {
        let mut f = StakeFlags::empty();

        // tier 8: would be masked to 0 by & 0x07, but > 5 so clamped to 0 first
        f.set_passive_tier(8);
        assert_eq!(f.passive_tier(), 0, "tier 8 should be clamped to 0");

        // tier 255: same clamping
        f.set_passive_tier(255);
        assert_eq!(f.passive_tier(), 0, "tier 255 should be clamped to 0");

        // tier 13 (0b1101): would mask to 5 without clamping, but clamped to 0
        f.set_passive_tier(13);
        assert_eq!(f.passive_tier(), 0, "tier 13 should be clamped to 0");
    }

    #[test]
    fn test_flags_independence() {
        let mut f = StakeFlags::empty();

        // Set PERMANENTLY_LOCKED, then set passive tier — both should coexist
        f.set(StakeFlags::PERMANENTLY_LOCKED);
        f.set_passive_tier(3);
        assert!(
            f.contains(StakeFlags::PERMANENTLY_LOCKED),
            "PERMANENTLY_LOCKED should survive passive tier set"
        );
        assert_eq!(f.passive_tier(), 3, "passive tier should be 3");

        // Clear passive tier — PERMANENTLY_LOCKED should remain
        f.set_passive_tier(0);
        assert!(
            f.contains(StakeFlags::PERMANENTLY_LOCKED),
            "PERMANENTLY_LOCKED should survive passive tier clear"
        );
        assert_eq!(f.passive_tier(), 0);

        // Restore tier, then remove PERMANENTLY_LOCKED — tier should remain
        f.set_passive_tier(5);
        f.remove(StakeFlags::PERMANENTLY_LOCKED);
        assert!(
            !f.contains(StakeFlags::PERMANENTLY_LOCKED),
            "PERMANENTLY_LOCKED should be removed"
        );
        assert_eq!(
            f.passive_tier(),
            5,
            "passive tier should survive PERMANENTLY_LOCKED removal"
        );
    }
}
