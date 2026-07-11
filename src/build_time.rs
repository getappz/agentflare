use chrono::{DateTime, FixedOffset};
use std::sync::LazyLock as Lazy;

pub mod built_info {
    include!(concat!(env!("OUT_DIR"), "/built.rs"));
}

pub static BUILD_TIME: Lazy<DateTime<FixedOffset>> = Lazy::new(|| {
    DateTime::parse_from_rfc2822(built_info::BUILT_TIME_UTC)
        .expect("built_info::BUILT_TIME_UTC should always be a valid RFC2822 timestamp")
});

pub static TARGET: &str = built_info::TARGET;
