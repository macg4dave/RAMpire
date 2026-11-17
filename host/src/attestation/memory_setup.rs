//! What a VM's recorded setup holds of its definition: its RAM, and
//! nothing else. A restart, a new disk or a new network port changes
//! none of it, so none of them stops RAMpire resizing the VM.

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use quick_xml::XmlVersion;
use rampire_core::{format_byte_size, libvirt_unit_bytes};
use serde::{Deserialize, Serialize};

/// A VM's RAM as its live definition gives it. The VM's current size
/// (`<memory>`, `<currentMemory>`, the device's `requested` and `current`)
/// is what RAMpire moves, so it isn't part of it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemorySetup {
    /// `<maxMemory>`: the most the VM's memory may come to.
    pub max_memory_bytes: Option<u64>,
    pub max_memory_slots: Option<u32>,
    /// Each NUMA cell's memory, in order: what the VM boots with.
    pub numa_cells_bytes: Vec<u64>,
    /// RAMpire's virtio-mem device, the one with the VM's alias.
    pub device_size_bytes: u64,
    pub device_block_bytes: u64,
    pub device_node: Option<u32>,
    /// Every other memory device, as `model size`.
    pub other_memory_devices: Vec<String>,
    /// The memory balloon's model; `None` with no balloon.
    pub balloon: Option<String>,
    /// `<memoryBacking>` (locked memory, huge pages, its source), as its
    /// elements and attributes; `None` without one.
    pub memory_backing: Option<String>,
}

impl MemorySetup {
    /// The RAM setup in `xml`, with `alias` naming RAMpire's device.
    pub fn read(xml: &str, alias: &str) -> Result<Self, String> {
        let mut parse = Parse {
            alias,
            ..Parse::default()
        };
        let mut reader = Reader::from_str(xml);
        let mut buffer = Vec::new();
        loop {
            let event = reader
                .read_event_into(&mut buffer)
                .map_err(|error| format!("domain XML: {error}"))?;
            match event {
                Event::Start(element) => {
                    parse.look(&element)?;
                    parse.path.push(name(&element));
                }
                Event::Empty(element) => {
                    parse.look(&element)?;
                    parse.path.push(name(&element));
                    parse.close();
                }
                Event::Text(text) => {
                    let raw = text.decode().map_err(|error| error.to_string())?;
                    parse.text(raw.trim())?;
                }
                Event::End(_) => parse.close(),
                Event::Eof => break,
                _ => {}
            }
            buffer.clear();
        }
        if !parse.found {
            return Err(format!(
                "domain XML: no virtio-mem device with alias {alias}"
            ));
        }
        Ok(parse.setup)
    }

    /// What changed from `self`, the recorded setup, to `live`, as a
    /// person would say it; `None` when nothing did.
    pub fn changed(&self, live: &Self) -> Option<String> {
        let size = |value: Option<u64>| value.map_or_else(|| "none".to_owned(), format_byte_size);
        let text = |value: &Option<String>| value.clone().unwrap_or_else(|| "none".to_owned());
        let list = |values: &[String]| {
            if values.is_empty() {
                "none".to_owned()
            } else {
                values.join(", ")
            }
        };
        let cells = |values: &[u64]| {
            list(
                &values
                    .iter()
                    .map(|&bytes| format_byte_size(bytes))
                    .collect::<Vec<_>>(),
            )
        };
        let differences = [
            (
                "its maximum memory",
                size(self.max_memory_bytes),
                size(live.max_memory_bytes),
            ),
            (
                "its memory slots",
                self.max_memory_slots
                    .map_or("none".to_owned(), |n| n.to_string()),
                live.max_memory_slots
                    .map_or("none".to_owned(), |n| n.to_string()),
            ),
            (
                "its NUMA cells",
                cells(&self.numa_cells_bytes),
                cells(&live.numa_cells_bytes),
            ),
            (
                "its virtio-mem device",
                format_byte_size(self.device_size_bytes),
                format_byte_size(live.device_size_bytes),
            ),
            (
                "its virtio-mem device's blocks",
                format_byte_size(self.device_block_bytes),
                format_byte_size(live.device_block_bytes),
            ),
            (
                "its virtio-mem device's NUMA node",
                self.device_node
                    .map_or("none".to_owned(), |n| n.to_string()),
                live.device_node
                    .map_or("none".to_owned(), |n| n.to_string()),
            ),
            (
                "its other memory devices",
                list(&self.other_memory_devices),
                list(&live.other_memory_devices),
            ),
            (
                "its memory balloon",
                text(&self.balloon),
                text(&live.balloon),
            ),
            (
                "its memory backing",
                text(&self.memory_backing),
                text(&live.memory_backing),
            ),
        ];
        differences
            .into_iter()
            .find(|(_, recorded, now)| recorded != now)
            .map(|(what, recorded, now)| format!("{what} went from {recorded} to {now}"))
    }
}

#[derive(Debug, Default)]
struct Device {
    model: String,
    alias: Option<String>,
    size: u64,
    block: u64,
    node: Option<u32>,
    reading: Option<Field>,
}

#[derive(Debug, Clone, Copy)]
enum Field {
    Size(Option<u64>),
    Block(Option<u64>),
    Node,
}

fn name(element: &BytesStart<'_>) -> String {
    String::from_utf8_lossy(element.name().as_ref()).into_owned()
}

fn attribute(element: &BytesStart<'_>, key: &str) -> Result<Option<String>, String> {
    for attribute in element.attributes() {
        let attribute = attribute.map_err(|error| format!("domain XML: {error}"))?;
        if attribute.key.as_ref() == key.as_bytes() {
            let value = attribute
                .normalized_value(XmlVersion::Explicit1_0)
                .map_err(|error| format!("domain XML: {error}"))?;
            return Ok(Some(value.into_owned()));
        }
    }
    Ok(None)
}

/// A unit's size in bytes; KiB when none is given, as libvirt reads it.
fn unit(element: &BytesStart<'_>) -> Result<Option<u64>, String> {
    let unit = attribute(element, "unit")?;
    libvirt_unit_bytes(unit.as_deref())
        .map(Some)
        .ok_or_else(|| format!("domain XML: unknown unit {unit:?}"))
}

fn bytes(value: &str, unit: Option<u64>) -> Result<u64, String> {
    value
        .parse::<u64>()
        .ok()
        .and_then(|number| number.checked_mul(unit.unwrap_or(1024)))
        .ok_or_else(|| format!("domain XML: memory size {value:?}"))
}

/// [`MemorySetup::read`] as it goes through the XML.
#[derive(Default)]
struct Parse<'a> {
    alias: &'a str,
    setup: MemorySetup,
    path: Vec<String>,
    /// `<maxMemory>`'s unit, until its text is read.
    max_unit: Option<u64>,
    /// The memory device being read.
    device: Option<Device>,
    /// What `<memoryBacking>` holds so far, while inside it.
    backing: Option<Vec<String>>,
    /// Whether RAMpire's device was found.
    found: bool,
}

impl Parse<'_> {
    fn parent(&self) -> Option<&str> {
        self.path.last().map(String::as_str)
    }

    fn look(&mut self, element: &BytesStart<'_>) -> Result<(), String> {
        let element_name = name(element);
        if let Some(parts) = self.backing.as_mut() {
            // Everything inside <memoryBacking>, with its attributes in order.
            let mut attributes = Vec::new();
            for attribute in element.attributes() {
                let attribute = attribute.map_err(|error| format!("domain XML: {error}"))?;
                let value = attribute
                    .normalized_value(XmlVersion::Explicit1_0)
                    .map_err(|error| format!("domain XML: {error}"))?;
                attributes.push(format!(
                    "{}={value}",
                    String::from_utf8_lossy(attribute.key.as_ref())
                ));
            }
            parts.push(if attributes.is_empty() {
                element_name
            } else {
                format!("{element_name}({})", attributes.join(" "))
            });
            return Ok(());
        }
        let parent = self.parent().map(str::to_owned);
        match (element_name.as_str(), parent.as_deref()) {
            ("maxMemory", Some("domain")) => {
                self.setup.max_memory_slots =
                    attribute(element, "slots")?.and_then(|slots| slots.parse().ok());
                self.max_unit = unit(element)?;
            }
            ("cell", Some("numa")) => {
                if let Some(memory) = attribute(element, "memory")? {
                    let cell = bytes(&memory, unit(element)?)?;
                    self.setup.numa_cells_bytes.push(cell);
                }
            }
            ("memory", Some("devices")) => {
                self.device = Some(Device {
                    model: attribute(element, "model")?.unwrap_or_default(),
                    ..Device::default()
                });
            }
            ("memballoon", Some("devices")) => self.setup.balloon = attribute(element, "model")?,
            ("memoryBacking", Some("domain")) => self.backing = Some(Vec::new()),
            (child, parent) => {
                let reading = match (child, parent) {
                    ("size", Some("target")) => Some(Field::Size(unit(element)?)),
                    ("block", Some("target")) => Some(Field::Block(unit(element)?)),
                    ("node", Some("target")) => Some(Field::Node),
                    _ => None,
                };
                if let Some(device) = self.device.as_mut() {
                    if reading.is_some() {
                        device.reading = reading;
                    } else if (child, parent) == ("alias", Some("memory")) {
                        device.alias = attribute(element, "name")?;
                    }
                }
            }
        }
        Ok(())
    }

    fn text(&mut self, value: &str) -> Result<(), String> {
        if self.path.len() == 2 && self.parent() == Some("maxMemory") {
            self.setup.max_memory_bytes = Some(bytes(value, self.max_unit)?);
        } else if let Some(device) = self.device.as_mut() {
            match device.reading.take() {
                Some(Field::Size(unit)) => device.size = bytes(value, unit)?,
                Some(Field::Block(unit)) => device.block = bytes(value, unit)?,
                Some(Field::Node) => device.node = value.parse().ok(),
                None => {}
            }
        }
        Ok(())
    }

    /// The element on top of the path ends.
    fn close(&mut self) {
        let Some(ended) = self.path.pop() else {
            return;
        };
        match (ended.as_str(), self.parent()) {
            ("memory", Some("devices")) => {
                if let Some(device) = self.device.take() {
                    if device.model == "virtio-mem" && device.alias.as_deref() == Some(self.alias) {
                        self.setup.device_size_bytes = device.size;
                        self.setup.device_block_bytes = device.block;
                        self.setup.device_node = device.node;
                        self.found = true;
                    } else {
                        self.setup.other_memory_devices.push(format!(
                            "{} {}",
                            device.model,
                            format_byte_size(device.size)
                        ));
                    }
                }
            }
            ("memoryBacking", Some("domain")) => {
                if let Some(parts) = self.backing.take() {
                    self.setup.memory_backing = Some(if parts.is_empty() {
                        "empty".to_owned()
                    } else {
                        parts.join(", ")
                    });
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;
    const MIB: u64 = 1 << 20;

    /// torrent101 on snow101 as libvirt showed it running on 2026-11-17.
    const TORRENT101: &str = "<domain type='kvm' id='32'>
  <name>torrent101</name>
  <maxMemory slots='1' unit='KiB'>16777216</maxMemory>
  <memory unit='KiB'>6291456</memory>
  <currentMemory unit='KiB'>6291456</currentMemory>
  <vcpu placement='static' current='2'>4</vcpu>
  <cpu mode='host-passthrough' check='none' migratable='on'>
    <numa>
      <cell id='0' cpus='0-3' memory='524288' unit='KiB'/>
    </numa>
  </cpu>
  <devices>
    <disk type='file' device='disk'>
      <source file='/var/lib/libvirt/images/torrent101.qcow2' index='3'/>
      <target dev='vda' bus='virtio'/>
    </disk>
    <interface type='bridge'>
      <mac address='52:54:00:10:63:40'/>
      <source network='lan' portid='94593afc-6de8-4ec8-b474-671d649abd7b' bridge='br0'/>
      <target dev='vnet378'/>
    </interface>
    <console type='pty' tty='/dev/pts/2'>
      <source path='/dev/pts/2'/>
    </console>
    <channel type='unix'>
      <source mode='bind' path='/run/libvirt/qemu/channel/32-torrent101/org.rampire.control.0'/>
      <target type='virtio' name='org.rampire.control.0' state='connected'/>
    </channel>
    <memballoon model='none'/>
    <memory model='virtio-mem'>
      <target>
        <size unit='KiB'>5767168</size>
        <node>0</node>
        <block unit='KiB'>2048</block>
        <requested unit='KiB'>0</requested>
        <current unit='KiB'>0</current>
        <address base='0x100000000'/>
      </target>
      <alias name='ua-virtiomem0'/>
    </memory>
  </devices>
  <seclabel type='dynamic' model='selinux' relabel='yes'>
    <label>system_u:system_r:svirt_t:s0:c412,c432</label>
  </seclabel>
</domain>";

    #[test]
    fn reads_the_ram_and_nothing_else() {
        let setup = MemorySetup::read(TORRENT101, "ua-virtiomem0").unwrap();
        assert_eq!(
            setup,
            MemorySetup {
                max_memory_bytes: Some(16 * GIB),
                max_memory_slots: Some(1),
                numa_cells_bytes: vec![512 * MIB],
                device_size_bytes: 5632 * MIB,
                device_block_bytes: 2 * MIB,
                device_node: Some(0),
                other_memory_devices: Vec::new(),
                balloon: Some("none".to_owned()),
                memory_backing: None,
            }
        );
    }

    #[test]
    fn a_restart_or_a_change_outside_ram_changes_nothing() {
        // torrent101 was refused every resize after a backup script
        // restarted it: its network port, tap, pty, channel folder, domain
        // id and security label were new. None of them is its RAM, and
        // neither is its size now, a new disk or a new MAC address.
        let recorded = MemorySetup::read(TORRENT101, "ua-virtiomem0").unwrap();
        let restarted = TORRENT101
            .replace("id='32'", "id='33'")
            .replace("94593afc-6de8-4ec8-b474-671d649abd7b", "102baa32-1c7d-442b-8d07-4718902067a5")
            .replace("vnet378", "vnet415")
            .replace("/dev/pts/2", "/dev/pts/5")
            .replace("32-torrent101", "33-torrent101")
            .replace("c412,c432", "c162,c634")
            .replace("<requested unit='KiB'>0</requested>", "<requested unit='KiB'>116736</requested>")
            .replace("<current unit='KiB'>0</current>", "<current unit='KiB'>116736</current>")
            .replace("<currentMemory unit='KiB'>6291456", "<currentMemory unit='KiB'>6408192")
            .replace("52:54:00:10:63:40", "52:54:00:10:63:41")
            .replace(
                "<memballoon",
                "<disk type='file' device='disk'><source file='/x.qcow2'/><target dev='vdb'/></disk><memballoon",
            );
        let now = MemorySetup::read(&restarted, "ua-virtiomem0").unwrap();
        assert_eq!(recorded.changed(&now), None);
    }

    #[test]
    fn a_change_to_the_ram_is_said() {
        let recorded = MemorySetup::read(TORRENT101, "ua-virtiomem0").unwrap();
        let said = |from: &str, to: &str| {
            recorded.changed(
                &MemorySetup::read(&TORRENT101.replace(from, to), "ua-virtiomem0").unwrap(),
            )
        };
        assert_eq!(
            said("16777216</maxMemory>", "8388608</maxMemory>").as_deref(),
            Some("its maximum memory went from 16GB to 8GB")
        );
        assert_eq!(
            said("memory='524288'", "memory='1048576'").as_deref(),
            Some("its NUMA cells went from 512MB to 1GB")
        );
        assert_eq!(
            said("<size unit='KiB'>5767168", "<size unit='KiB'>8388608").as_deref(),
            Some("its virtio-mem device went from 5632MB to 8GB")
        );
        assert_eq!(
            said("<memballoon model='none'/>", "<memballoon model='virtio'/>").as_deref(),
            Some("its memory balloon went from none to virtio")
        );
        assert_eq!(
            said("<devices>", "<devices><memory model='dimm'><target><size unit='KiB'>1048576</size><node>0</node></target></memory>").as_deref(),
            Some("its other memory devices went from none to dimm 1GB")
        );
        assert_eq!(
            said("<vcpu", "<memoryBacking><locked/><hugepages><page size='2048' unit='KiB'/></hugepages></memoryBacking><vcpu").as_deref(),
            Some("its memory backing went from none to locked, hugepages, page(size=2048 unit=KiB)")
        );
        // RAMpire's device is the one with the VM's alias.
        assert!(MemorySetup::read(TORRENT101, "ua-other").is_err());
    }
}
