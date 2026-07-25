pub mod capture;
pub mod classify;
pub mod consolidate;
pub mod file;
pub mod paths;

pub fn event_id(message: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    nanos.hash(&mut h);
    message.hash(&mut h);
    format!("{:08x}", h.finish() as u32)
}
