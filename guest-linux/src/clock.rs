//! Linux's time since boot, for the shared worker's clock.

/// Time since boot from `/proc/uptime`; `None` when it can't be read, which
/// the publication reports as unknown.
pub fn boot_uptime_millis() -> Option<u64> {
    let bytes =
        rampire_guest::files::read_bounded(std::path::Path::new("/proc/uptime"), 256).ok()??;
    let text = String::from_utf8(bytes).ok()?;
    let (seconds, fraction) = text.split_ascii_whitespace().next()?.split_once('.')?;
    let centiseconds = fraction.get(..2)?.parse::<u64>().ok()?;
    seconds
        .parse::<u64>()
        .ok()?
        .checked_mul(1000)?
        .checked_add(centiseconds * 10)
}
