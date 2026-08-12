//! Host resource awareness for autonomous dispatch.
//!
//! Two related but independent concerns live here, both answering "how much
//! can this host handle right now":
//!   * [`pool_size`] — one-shot CPU+memory sizing for a worker pool at
//!     startup (`work_max_concurrency`).
//!   * [`gate`] — a live, continuously-sampled CPU-pressure tier
//!     ([`Policy`]) that dispatch loops consult on every tick.
//!
//! This is deliberately separate from `auth_db`'s per-agent rate-limit
//! cooldown, which is a different domain (agent identity, not host
//! pressure) — a caller gates dispatch on both independently.

pub mod config;
pub mod gate;
pub mod policy;
pub mod pool_size;
pub mod signals;

pub use config::{GateConfig, GateMode};
pub use gate::{current_policy, init_global};
pub use policy::{PauseReason, Policy};
pub use signals::Signals;
