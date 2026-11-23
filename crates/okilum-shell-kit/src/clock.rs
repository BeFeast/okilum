//! Wall-clock helpers shared by kit and app modules.

/// Seconds since the Unix epoch; 0 if the clock is before it.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}
