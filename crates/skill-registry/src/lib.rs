pub mod builtin;
pub mod db;
pub mod embed_store;
pub mod frontmatter;
pub mod hub;
pub mod load;
pub mod pack;
pub mod search;
pub mod sources;

pub use load::{LoadError, LoadedSkill, Registry, load};
pub use pack::SkillBundle;
pub use search::{MatchMode, SkillHit, merge_registry_hits, search};
