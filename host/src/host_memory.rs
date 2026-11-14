//! Host-side (not guest-side) available-memory gate. Growing the virtio-mem
//! device consumes host RAM; a grow request must never be sent unless the
//! host itself has enough free memory left over after the reserved headroom.

use std::fs;

use rampire_core::meminfo_bytes;

/// Admission leaves slack above the hard reserve for host activity between
/// observations. The final effect-boundary check still enforces the reserve.
pub(crate) const GROW_ADMISSION_MARGIN_BYTES: u64 = 256 * 1024 * 1024;

pub trait HostMemorySource {
    fn available_bytes(&self) -> Result<u64, String>;
}

pub(crate) fn validate_grow_headroom(
    current_bytes: u64,
    target_bytes: u64,
    available_bytes: u64,
    minimum_headroom_bytes: u64,
) -> Result<(), String> {
    if target_bytes <= current_bytes {
        return Ok(());
    }
    let grow_bytes = target_bytes - current_bytes;
    if grow_bytes > available_bytes || available_bytes - grow_bytes < minimum_headroom_bytes {
        return Err(format!(
            "host headroom is insufficient: available={available_bytes} grow={grow_bytes} minimum_headroom={minimum_headroom_bytes}"
        ));
    }
    Ok(())
}

pub struct ProcMeminfoSource {
    path: String,
}

impl ProcMeminfoSource {
    pub fn new() -> Self {
        Self {
            path: "/proc/meminfo".to_owned(),
        }
    }
}

impl Default for ProcMeminfoSource {
    fn default() -> Self {
        Self::new()
    }
}

impl HostMemorySource for ProcMeminfoSource {
    fn available_bytes(&self) -> Result<u64, String> {
        let contents = fs::read_to_string(&self.path)
            .map_err(|error| format!("failed to read {}: {error}", self.path))?;
        meminfo_bytes(&contents, "MemAvailable")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grow_headroom_accepts_exact_reserve_and_shrinks() {
        assert_eq!(validate_grow_headroom(4, 6, 5, 3), Ok(()));
        assert_eq!(validate_grow_headroom(6, 4, 0, 3), Ok(()));
        assert_eq!(validate_grow_headroom(4, 4, 0, 3), Ok(()));
    }

    #[test]
    fn grow_headroom_rejects_reserve_shortfall_and_growth_beyond_available() {
        assert!(validate_grow_headroom(4, 6, 4, 3).is_err());
        assert!(validate_grow_headroom(4, 10, 5, 0).is_err());
    }
}
