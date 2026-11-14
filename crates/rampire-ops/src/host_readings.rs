//! What the live views read from the host on every refresh: its memory
//! from /proc/meminfo and a VM's virtio-mem device from its live
//! definition, with the bounds every view's reads share.

use std::time::Duration;

use rampire_core::{parse_virtio_mem_xml_for_alias, VirtioMemState};
use rampire_host::host_memory::meminfo_bytes;
use rampire_host::virsh::{Virsh, VirshCommand};

/// The largest guest publication a view reads.
pub const MAX_TELEMETRY_BYTES: usize = 64 * 1024;
/// How long one virsh or journalctl read may take.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HostMemory {
    pub total: u64,
    pub available: u64,
    pub swap_total: u64,
    pub swap_free: u64,
}

pub fn read_meminfo() -> Result<HostMemory, String> {
    let text = std::fs::read_to_string("/proc/meminfo")
        .map_err(|error| format!("read /proc/meminfo: {error}"))?;
    Ok(parse_meminfo(&text))
}

fn parse_meminfo(text: &str) -> HostMemory {
    let field = |name: &str| meminfo_bytes(text, name).unwrap_or(0);
    HostMemory {
        total: field("MemTotal"),
        available: field("MemAvailable"),
        swap_total: field("SwapTotal"),
        swap_free: field("SwapFree"),
    }
}

pub fn device_state(virsh: &Virsh, vm: &str, alias: &str) -> Option<VirtioMemState> {
    let xml = virsh.run(&["dumpxml".to_owned(), vm.to_owned()]).ok()?;
    parse_virtio_mem_xml_for_alias(&xml, alias)
        .ok()
        .map(|state| state.memory)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_meminfo_fields_in_bytes() {
        let host = parse_meminfo(
            "MemTotal:       32222876 kB\nMemFree: 1 kB\nMemAvailable:    8068956 kB\nSwapTotal: 4 kB\nSwapFree: 1 kB\n",
        );
        assert_eq!(host.total, 32_222_876 * 1024);
        assert_eq!(host.available, 8_068_956 * 1024);
        assert_eq!((host.swap_total, host.swap_free), (4096, 1024));
    }
}
