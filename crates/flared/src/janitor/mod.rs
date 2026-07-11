//! File-level cleanup plugins that run on the deep sweep. Each janitor
//! reports what it would do; mutation requires an explicit execute call and
//! always writes a backup first.

pub mod lean_ctx;
