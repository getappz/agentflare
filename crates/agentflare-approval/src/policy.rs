//! Autonomy-tier policy: maps a ([`AutonomyTier`], [`CommandClass`]) pair to
//! a [`TierAction`]. See the epic's tier matrix:
//!
//! | Tier       | Read  | Write  | Network / Install / Destructive |
//! |------------|-------|--------|----------------------------------|
//! | ReadOnly   | Allow | Block  | Block                            |
//! | Supervised | Allow | Prompt | Prompt                          |
//! | Full       | Allow | Allow  | Prompt                          |
//!
//! `Full` never allows `Network`/`Install`/`Destructive` silently — those
//! three stay `Prompt` in every tier except the (nonexistent) tier that
//! would auto-allow them. There is deliberately no such tier.

use crate::types::{AutonomyTier, CommandClass, TierAction};

pub fn tier_action(tier: AutonomyTier, class: CommandClass) -> TierAction {
    match (tier, class) {
        (_, CommandClass::Read) => TierAction::Allow,

        (AutonomyTier::ReadOnly, _) => TierAction::Block,

        (AutonomyTier::Supervised, CommandClass::Write) => TierAction::Prompt,
        (AutonomyTier::Full, CommandClass::Write) => TierAction::Allow,

        (_, CommandClass::Network | CommandClass::Install | CommandClass::Destructive) => {
            TierAction::Prompt
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use CommandClass::*;

    #[test]
    fn read_only_tier_allows_read_and_blocks_everything_else() {
        assert_eq!(tier_action(AutonomyTier::ReadOnly, Read), TierAction::Allow);
        for class in [Write, Network, Install, Destructive] {
            assert_eq!(
                tier_action(AutonomyTier::ReadOnly, class),
                TierAction::Block,
                "{class:?} must be blocked in ReadOnly tier"
            );
        }
    }

    #[test]
    fn supervised_tier_prompts_on_everything_but_read() {
        assert_eq!(
            tier_action(AutonomyTier::Supervised, Read),
            TierAction::Allow
        );
        for class in [Write, Network, Install, Destructive] {
            assert_eq!(
                tier_action(AutonomyTier::Supervised, class),
                TierAction::Prompt,
                "{class:?} must prompt in Supervised tier"
            );
        }
    }

    #[test]
    fn full_tier_allows_read_and_write_but_still_prompts_risky_classes() {
        assert_eq!(tier_action(AutonomyTier::Full, Read), TierAction::Allow);
        assert_eq!(tier_action(AutonomyTier::Full, Write), TierAction::Allow);
        for class in [Network, Install, Destructive] {
            assert_eq!(
                tier_action(AutonomyTier::Full, class),
                TierAction::Prompt,
                "{class:?} must still prompt in Full tier"
            );
        }
    }

    #[test]
    fn block_is_never_produced_by_a_tier_other_than_read_only() {
        for tier in [AutonomyTier::Supervised, AutonomyTier::Full] {
            for class in [Read, Write, Network, Install, Destructive] {
                assert_ne!(tier_action(tier, class), TierAction::Block);
            }
        }
    }
}
