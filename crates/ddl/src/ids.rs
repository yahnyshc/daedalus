use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn next_id(prefix: &str) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let suffix = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}_{:x}{suffix:04x}", now.as_micros())
}
