pub const SKILL_REVIEW: &str = include_str!("skill-review.md");
pub const SKILL_AUDIT: &str = include_str!("skill-audit.md");
pub const SKILL_DEBT: &str = include_str!("skill-debt.md");
pub const SKILL_GAIN: &str = include_str!("skill-gain.md");
pub const SKILL_HELP: &str = include_str!("skill-help.md");

pub fn get(name: &str) -> Option<&'static str> {
    match name {
        "review" => Some(SKILL_REVIEW),
        "audit" => Some(SKILL_AUDIT),
        "debt" => Some(SKILL_DEBT),
        "gain" => Some(SKILL_GAIN),
        "help" => Some(SKILL_HELP),
        _ => None,
    }
}
