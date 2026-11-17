//! `rampire vm add` and `rampire vm remove`:
//! read a domain's definition, say what it lacks, fix what can be fixed by
//! hand-checked text edits, and suggest its settings and review.

use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};

use rampire_core::{format_byte_size, libvirt_unit_bytes, GIB, MEMORY_CONTROL_CHANNEL_NAME, MIB};
use rampire_host::attestation::AnswerSource;

/// A virtio-mem device in a domain's definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub alias: Option<String>,
    pub size_bytes: u64,
    pub block_bytes: u64,
    pub requested_bytes: u64,
}

/// The QEMU guest agent's virtio-serial channel.
pub const GUEST_AGENT_CHANNEL_NAME: &str = "org.qemu.guest_agent.0";

/// What `vm add` reads from a domain's definition.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inspection {
    /// The memory of the NUMA cells: the fixed boot RAM.
    pub boot_bytes: u64,
    pub numa_cells: usize,
    /// `maxMemory`'s slots, when set.
    pub max_memory_slots: Option<u32>,
    /// `maxMemory`'s size, when set: libvirt refuses a definition whose
    /// NUMA cells and memory devices come to more.
    pub max_memory_bytes: Option<u64>,
    max_memory_unit: Option<String>,
    /// Other memory devices (DIMMs and the like) that use slots too.
    pub other_memory_devices: usize,
    pub devices: Vec<Device>,
    pub control_channel: bool,
    /// The QEMU guest agent's channel, through which `vm add` reads the
    /// guest's kernel release.
    pub guest_agent_channel: bool,
    /// Passed-through devices that may pin guest memory: PCI and mediated
    /// (VFIO) ones, not USB or SCSI.
    pub hostdevs: usize,
    pub vdpa: bool,
    pub vhost_user: bool,
    pub launch_security: bool,
    pub locked_memory: bool,
    /// The memory balloon's model; `None` with no balloon element.
    pub balloon: Option<String>,
    /// `<vcpu>`: the virtual CPUs a NUMA cell added by `vm add` holds.
    pub vcpus: u32,
    /// The domain's `<memory>`: with no NUMA cell, the boot RAM.
    pub memory_bytes: u64,
    memory_unit: Option<String>,
    /// `<currentMemory>`: what a balloon holds the VM at, when set.
    pub current_memory_bytes: Option<u64>,
    current_memory_unit: Option<String>,
}

impl Inspection {
    pub fn device(&self) -> Option<&Device> {
        self.devices.first()
    }

    /// Whether `live`, the running definition, lacks what this saved one
    /// gives RAMpire: the device, the control channel or the balloon as
    /// saved. Such a VM needs a cold start to run its saved definition.
    pub fn runs_behind(&self, live: &Self) -> bool {
        live.device().is_none()
            || live.control_channel != self.control_channel
            || live.guest_agent_channel != self.guest_agent_channel
            || live.balloon != self.balloon
    }

    /// Problems that stop RAMpire managing the domain, each with its fix.
    pub fn problems(&self) -> Vec<(String, String)> {
        let mut problems = Vec::new();
        if self.numa_cells == 0 {
            problems.push((
                "the domain has no NUMA cell".to_owned(),
                "rampire vm add adds one, or add <numa><cell id='0' cpus='0-N' memory='BOOT' unit='KiB'/></numa> inside <cpu>".to_owned(),
            ));
        }
        if self.max_memory_slots.is_none() {
            problems.push((
                "the domain has no <maxMemory>".to_owned(),
                "rampire vm add adds it, or add <maxMemory slots='1' unit='KiB'>TOTAL</maxMemory>, TOTAL being boot RAM plus the device".to_owned(),
            ));
        }
        match self.devices.len() {
            0 => problems.push((
                "the domain has no virtio-mem device".to_owned(),
                "rampire vm add adds one".to_owned(),
            )),
            1 => {}
            _ => problems.push((
                "the domain has more than one virtio-mem device".to_owned(),
                "RAMpire manages one device per VM: remove the others".to_owned(),
            )),
        }
        if let Some(device) = self.device() {
            if device
                .alias
                .as_deref()
                .is_none_or(|alias| !alias.starts_with("ua-"))
            {
                problems.push((
                    "the virtio-mem device has no user alias".to_owned(),
                    "give it <alias name='ua-virtiomem0'/>, so its name is stable".to_owned(),
                ));
            }
        }
        problems
    }

    /// Whether a change [`Inspection::fixable`] names takes effect only at
    /// the VM's next start: every one but saving the device's requested
    /// size, which a running VM doesn't read. A running VM is then shut
    /// down and started again by `vm add`.
    pub fn fixes_take_a_restart(&self) -> bool {
        !self.control_channel
            || !self.guest_agent_channel
            || (self.device().is_none() && self.can_add_device())
            || self.balloon_active()
    }

    /// Changes `vm add` makes to the definition, in words.
    pub fn fixable(&self) -> Vec<String> {
        let mut changes = Vec::new();
        if !self.control_channel {
            changes.push(format!(
                "add the control channel {MEMORY_CONTROL_CHANNEL_NAME}"
            ));
        }
        if !self.guest_agent_channel {
            changes.push(format!(
                "add the guest agent channel {GUEST_AGENT_CHANNEL_NAME}"
            ));
        }
        match self.device() {
            Some(device) if device.requested_bytes != 0 => changes.push(
                "save the virtio-mem device's requested size as 0, so it boots on boot RAM"
                    .to_owned(),
            ),
            None if self.can_add_device() => {
                if self.adds_numa_cell() {
                    changes.push(format!(
                        "add a NUMA cell: {} vCPUs, {} MiB boot RAM",
                        self.vcpus,
                        self.boot() / MIB
                    ));
                }
                if self.max_memory_slots.is_none() {
                    let slots = self.other_memory_devices + 1;
                    changes.push(format!(
                        "add <maxMemory>, {slots} slot{}",
                        if slots == 1 { "" } else { "s" }
                    ));
                }
                changes.push(format!(
                    "add a {} MiB virtio-mem device",
                    default_device_bytes(self.boot()) / MIB
                ));
            }
            _ => {}
        }
        if self.balloon_active() {
            let mut change = format!(
                "turn off the memory balloon ({}); it would fight virtio-mem",
                self.balloon.as_deref().unwrap_or_default()
            );
            // A balloon target below boot RAM goes with the balloon: say so,
            // since the VM then takes more of the host's memory.
            if let Some(current) = self
                .current_memory_bytes
                .filter(|current| *current < self.boot())
            {
                change.push_str(&format!(
                    ". The VM then uses all {1} MiB of its boot RAM, not the balloon's {0} MiB",
                    current / MIB,
                    self.boot() / MIB
                ));
            }
            changes.push(change);
        }
        changes
    }

    /// A memory balloon that could resize the VM: any model but `none`.
    fn balloon_active(&self) -> bool {
        self.balloon.as_deref().is_some_and(|model| model != "none")
    }

    /// Boot RAM: the NUMA cells' memory, or the domain's `<memory>` when a
    /// cell is still to be added.
    fn boot(&self) -> u64 {
        if self.numa_cells > 0 {
            self.boot_bytes
        } else {
            self.memory_bytes
        }
    }

    /// No NUMA cell, but what one needs: the vCPUs and the memory.
    fn adds_numa_cell(&self) -> bool {
        self.numa_cells == 0 && self.vcpus > 0 && self.memory_bytes > 0
    }

    /// A device can be added once there is a NUMA cell and a free memory
    /// slot, or when `vm add` can add them: a cell when there is none,
    /// and `<maxMemory>` when it is missing. Too few slots in an existing
    /// `<maxMemory>` is left to a person.
    fn can_add_device(&self) -> bool {
        (self.numa_cells > 0 || self.adds_numa_cell())
            && self
                .max_memory_slots
                .is_none_or(|slots| slots as usize > self.other_memory_devices)
    }
}

/// What the running `vm` starts with, the memory of its NUMA cell, from
/// its live definition; `None` when libvirt can't say or it has no cell.
pub fn boot_bytes(libvirt: &crate::attest::Libvirt, vm: &str) -> Option<u64> {
    let live = inspect(&libvirt.virsh(&["dumpxml", vm]).ok()?).ok()?;
    (live.numa_cells > 0).then_some(live.boot_bytes)
}

/// Whether the running `vm` lacks what its saved definition gives RAMpire
/// ([`Inspection::runs_behind`]), so it needs a cold start; `None` when
/// libvirt can't say.
pub fn runs_behind(libvirt: &crate::attest::Libvirt, vm: &str) -> Option<bool> {
    let saved = inspect(&libvirt.virsh(&["dumpxml", "--inactive", vm]).ok()?).ok()?;
    let live = inspect(&libvirt.virsh(&["dumpxml", vm]).ok()?).ok()?;
    Some(saved.runs_behind(&live))
}

/// A new device's size: three times boot RAM, so a Linux guest grows to
/// about four times its boot RAM and can still return it reliably. Rounded
/// up to whole GiB, at most the largest whole GiB a u64 holds.
pub fn default_device_bytes(boot_bytes: u64) -> u64 {
    boot_bytes
        .saturating_mul(3)
        .div_ceil(GIB)
        .min(u64::MAX / GIB)
        * GIB
}

/// Read a domain's definition.
pub fn inspect(xml: &str) -> Result<Inspection, String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut path: Vec<Vec<u8>> = Vec::new();
    let mut inspection = Inspection::default();
    let mut device: Option<(Device, Option<&'static str>, Option<String>)> = None;
    loop {
        let event = reader
            .read_event_into(&mut buffer)
            .map_err(|error| format!("domain XML: {error}"))?;
        match event {
            Event::Start(element) => {
                look(&element, &path, &mut inspection, &mut device)?;
                path.push(element.name().as_ref().to_vec());
            }
            Event::Empty(element) => look(&element, &path, &mut inspection, &mut device)?,
            Event::Text(text) => {
                let at = |names: &[&[u8]]| {
                    path.len() == names.len()
                        && path
                            .iter()
                            .zip(names)
                            .all(|(seen, name)| seen.as_slice() == *name)
                };
                if at(&[b"domain", b"vcpu"]) {
                    let raw = text.decode().map_err(|error| error.to_string())?;
                    inspection.vcpus = raw
                        .trim()
                        .parse()
                        .map_err(|_| format!("domain XML: vcpu {raw:?}"))?;
                } else if at(&[b"domain", b"memory"]) {
                    let raw = text.decode().map_err(|error| error.to_string())?;
                    let unit = inspection.memory_unit.take();
                    inspection.memory_bytes = libvirt_bytes(raw.trim(), unit.as_deref())?;
                } else if at(&[b"domain", b"maxMemory"]) {
                    let raw = text.decode().map_err(|error| error.to_string())?;
                    let unit = inspection.max_memory_unit.take();
                    inspection.max_memory_bytes = Some(libvirt_bytes(raw.trim(), unit.as_deref())?);
                } else if at(&[b"domain", b"currentMemory"]) {
                    let raw = text.decode().map_err(|error| error.to_string())?;
                    let unit = inspection.current_memory_unit.take();
                    inspection.current_memory_bytes =
                        Some(libvirt_bytes(raw.trim(), unit.as_deref())?);
                }
                if let Some((current, field, unit)) = device.as_mut() {
                    if let Some(field) = field.take() {
                        let raw = text.decode().map_err(|error| error.to_string())?;
                        let bytes = libvirt_bytes(raw.trim(), unit.as_deref())?;
                        match field {
                            "size" => current.size_bytes = bytes,
                            "block" => current.block_bytes = bytes,
                            _ => current.requested_bytes = bytes,
                        }
                    }
                }
            }
            Event::End(element) => {
                path.pop();
                if element.name().as_ref() == b"memory"
                    && path.last().map(Vec::as_slice) == Some(b"devices")
                {
                    if let Some((finished, _, _)) = device.take() {
                        inspection.devices.push(finished);
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    Ok(inspection)
}

fn look(
    element: &BytesStart<'_>,
    path: &[Vec<u8>],
    inspection: &mut Inspection,
    device: &mut Option<(Device, Option<&'static str>, Option<String>)>,
) -> Result<(), String> {
    let name = element.name().as_ref().to_vec();
    let parent = path.last().map(Vec::as_slice);
    match (name.as_slice(), parent) {
        (b"memory", Some(b"domain")) => {
            inspection.memory_unit = attribute(element, b"unit")?;
        }
        (b"currentMemory", Some(b"domain")) => {
            inspection.current_memory_unit = attribute(element, b"unit")?;
        }
        (b"maxMemory", Some(b"domain")) => {
            inspection.max_memory_slots =
                attribute(element, b"slots")?.and_then(|slots| slots.parse().ok());
            inspection.max_memory_unit = attribute(element, b"unit")?;
        }
        (b"cell", Some(b"numa")) => {
            inspection.numa_cells += 1;
            if let Some(memory) = attribute(element, b"memory")? {
                let unit = attribute(element, b"unit")?;
                inspection.boot_bytes = inspection
                    .boot_bytes
                    .checked_add(libvirt_bytes(&memory, unit.as_deref())?)
                    .ok_or("domain XML: the NUMA cells' memory overflows")?;
            }
        }
        (b"memory", Some(b"devices")) => {
            if attribute(element, b"model")?.as_deref() == Some("virtio-mem") {
                *device = Some((
                    Device {
                        alias: None,
                        size_bytes: 0,
                        block_bytes: 0,
                        requested_bytes: 0,
                    },
                    None,
                    None,
                ));
            } else {
                inspection.other_memory_devices += 1;
            }
        }
        (b"size" | b"block" | b"requested", Some(b"target")) => {
            if let Some((_, field, unit)) = device.as_mut() {
                *field = Some(match name.as_slice() {
                    b"size" => "size",
                    b"block" => "block",
                    _ => "requested",
                });
                *unit = attribute(element, b"unit")?;
            }
        }
        (b"alias", Some(b"memory")) => {
            if let Some((current, _, _)) = device.as_mut() {
                current.alias = attribute(element, b"name")?;
            }
        }
        (b"target", Some(b"channel")) => match attribute(element, b"name")?.as_deref() {
            Some(MEMORY_CONTROL_CHANNEL_NAME) => inspection.control_channel = true,
            Some(GUEST_AGENT_CHANNEL_NAME) => inspection.guest_agent_channel = true,
            _ => {}
        },
        // USB and SCSI devices are passed through by QEMU, which copies each
        // transfer: they map no guest memory, so they pin none. A PCI or
        // mediated (VFIO) device maps guest memory for its own DMA.
        (b"hostdev", Some(b"devices")) => {
            if !matches!(
                attribute(element, b"type")?.as_deref(),
                Some("usb" | "scsi")
            ) {
                inspection.hostdevs += 1;
            }
        }
        (b"interface", Some(b"devices")) => match attribute(element, b"type")?.as_deref() {
            Some("vdpa") => inspection.vdpa = true,
            Some("vhostuser") => inspection.vhost_user = true,
            _ => {}
        },
        (b"filesystem" | b"disk", Some(b"devices")) => {
            if attribute(element, b"type")?.as_deref() == Some("vhostuser") {
                inspection.vhost_user = true;
            }
        }
        (b"launchSecurity", Some(b"domain")) => inspection.launch_security = true,
        (b"locked", Some(b"memoryBacking")) => inspection.locked_memory = true,
        (b"memballoon", Some(b"devices")) => {
            inspection.balloon = Some(attribute(element, b"model")?.unwrap_or_default());
        }
        _ => {}
    }
    Ok(())
}

fn attribute(element: &BytesStart<'_>, key: &[u8]) -> Result<Option<String>, String> {
    for attribute in element.attributes().with_checks(false) {
        let attribute = attribute.map_err(|error| error.to_string())?;
        if attribute.key.as_ref() == key {
            return Ok(Some(
                attribute
                    .normalized_value(XmlVersion::Explicit1_0)
                    .map_err(|error| error.to_string())?
                    .into_owned(),
            ));
        }
    }
    Ok(None)
}

/// A libvirt memory size in bytes; `unit` `None` is libvirt's KiB.
fn libvirt_bytes(value: &str, unit: Option<&str>) -> Result<u64, String> {
    let value: u64 = value
        .parse()
        .map_err(|_| format!("domain XML: {value:?} is not a size"))?;
    let multiplier = libvirt_unit_bytes(unit)
        .ok_or_else(|| format!("domain XML: unknown unit {:?}", unit.unwrap_or_default()))?;
    value
        .checked_mul(multiplier)
        .ok_or_else(|| "domain XML: size overflows".to_owned())
}

/// The definition with every fix in [`Inspection::fixable`] made, or `None`
/// when there is nothing to fix. Edits are plain text, so everything else
/// in the definition stays exactly as it was.
pub fn fix_xml(xml: &str, inspection: &Inspection) -> Result<Option<String>, String> {
    let mut text = xml.to_owned();
    match inspection.device() {
        Some(device) if device.requested_bytes != 0 => {
            let start = text
                .find("<memory model='virtio-mem'")
                .ok_or("can't find the virtio-mem device's text")?;
            let end = start
                + text[start..]
                    .find("</memory>")
                    .ok_or("the virtio-mem device has no end")?;
            let block = &text[start..end];
            let open = block
                .find("<requested")
                .ok_or("no <requested> in the device")?;
            let close = block[open..]
                .find("</requested>")
                .ok_or("no </requested> in the device")?
                + open
                + "</requested>".len();
            let fixed = format!(
                "{}<requested unit='KiB'>0</requested>{}",
                &block[..open],
                &block[close..]
            );
            text.replace_range(start..end, &fixed);
        }
        None if inspection.can_add_device() => {
            let boot = inspection.boot();
            let size = default_device_bytes(boot);
            if inspection.adds_numa_cell() {
                insert_numa_cell(&mut text, inspection.vcpus, boot)?;
            }
            if inspection.max_memory_slots.is_none() {
                insert_max_memory(&mut text, inspection.other_memory_devices + 1, boot + size)?;
            }
            insert_device(
                &mut text,
                &format!(
                    "    <memory model='virtio-mem'>\n      <target>\n        <size unit='KiB'>{}</size>\n        <node>0</node>\n        <block unit='KiB'>2048</block>\n        <requested unit='KiB'>0</requested>\n      </target>\n      <alias name='ua-virtiomem0'/>\n    </memory>\n",
                    size / 1024
                ),
            )?;
        }
        _ => {}
    }
    if inspection.balloon_active() {
        turn_off_balloon(&mut text)?;
    }
    if !inspection.control_channel {
        insert_device(
            &mut text,
            &format!(
                "    <channel type='unix'>\n      <target type='virtio' name='{MEMORY_CONTROL_CHANNEL_NAME}'/>\n      <alias name='ua-rampire-control'/>\n    </channel>\n"
            ),
        )?;
    }
    if !inspection.guest_agent_channel {
        insert_device(
            &mut text,
            &format!(
                "    <channel type='unix'>\n      <target type='virtio' name='{GUEST_AGENT_CHANNEL_NAME}'/>\n    </channel>\n"
            ),
        )?;
    }
    Ok((text != xml).then_some(text))
}

/// A NUMA cell holding every vCPU and the boot RAM, in `<cpu>`; a domain
/// with no `<cpu>` gets one before `<devices>`.
fn insert_numa_cell(text: &mut String, vcpus: u32, boot: u64) -> Result<(), String> {
    let numa = format!(
        "<numa>\n      <cell id='0' cpus='0-{}' memory='{}' unit='KiB'/>\n    </numa>",
        vcpus - 1,
        boot / 1024
    );
    if let Some(start) = find_element(text, "cpu") {
        let close = start
            + text[start..]
                .find('>')
                .ok_or("the <cpu> element has no end")?;
        if text[..close].ends_with('/') {
            // <cpu .../> becomes <cpu ...>NUMA</cpu>.
            let open = text[start..close - 1].trim_end().to_owned();
            text.replace_range(start..=close, &format!("{open}>\n    {numa}\n  </cpu>"));
        } else {
            let end = start
                + text[start..]
                    .find("</cpu>")
                    .ok_or("the <cpu> element has no end")?;
            let line_start = text[..end].rfind('\n').map_or(end, |index| index + 1);
            text.insert_str(line_start, &format!("    {numa}\n"));
        }
        return Ok(());
    }
    let devices = find_element(text, "devices").ok_or("the domain has no <devices>")?;
    let line_start = text[..devices]
        .rfind('\n')
        .map_or(devices, |index| index + 1);
    text.insert_str(line_start, &format!("  <cpu>\n    {numa}\n  </cpu>\n"));
    Ok(())
}

/// Where the element `<name ...>` starts: the name followed by a space,
/// `>` or `/`, so `<cpu` doesn't find `<cputune>`.
fn find_element(text: &str, name: &str) -> Option<usize> {
    let open = format!("<{name}");
    let mut from = 0;
    while let Some(at) = text[from..].find(&open) {
        let start = from + at;
        match text[start + open.len()..].chars().next() {
            Some(' ' | '>' | '/' | '\n' | '\t') => return Some(start),
            _ => from = start + open.len(),
        }
    }
    None
}

/// The definition without what RAMpire needs, as `vm remove` leaves it:
/// no virtio-mem device or control channel, no `<maxMemory>` when no
/// other memory device is left, no NUMA cell when there is one (a single
/// cell only fixes the memory's size), `<memory>` and `<currentMemory>` the
/// boot RAM, and the memory balloon `original` (the definition before
/// `vm add`) had. The guest agent's channel stays: it is libvirt's own.
pub fn plain_xml(xml: &str, original: Option<&str>) -> Result<String, String> {
    let inspection = inspect(xml)?;
    let boot = inspection.boot();
    let mut text = xml.to_owned();
    while let Some(start) = text.find("<memory model='virtio-mem'") {
        let end = start
            + text[start..]
                .find("</memory>")
                .ok_or("the virtio-mem device has no end")?
            + "</memory>".len();
        remove_lines(&mut text, start, end);
    }
    if let Some(name) = text.find(&format!("name='{MEMORY_CONTROL_CHANNEL_NAME}'")) {
        let start = text[..name]
            .rfind("<channel")
            .ok_or("the control channel has no start")?;
        let end = name
            + text[name..]
                .find("</channel>")
                .ok_or("the control channel has no end")?
            + "</channel>".len();
        remove_lines(&mut text, start, end);
    }
    if inspection.other_memory_devices == 0 {
        if let Some(start) = find_element(&text, "maxMemory") {
            let end = start
                + text[start..]
                    .find("</maxMemory>")
                    .ok_or("<maxMemory> has no end")?
                + "</maxMemory>".len();
            remove_lines(&mut text, start, end);
        }
    }
    if inspection.numa_cells == 1 {
        if let Some(start) = find_element(&text, "numa") {
            let end =
                start + text[start..].find("</numa>").ok_or("<numa> has no end")? + "</numa>".len();
            remove_lines(&mut text, start, end);
        }
        // A bare <cpu> left empty says nothing.
        if let Some(start) = text.find("<cpu>") {
            let rest = &text[start + "<cpu>".len()..];
            if let Some(close) = rest.find("</cpu>") {
                if rest[..close].trim().is_empty() {
                    let end = start + "<cpu>".len() + close + "</cpu>".len();
                    remove_lines(&mut text, start, end);
                }
            }
        }
    }
    for name in ["memory", "currentMemory"] {
        if let Some(start) = find_element(&text, name) {
            let close = format!("</{name}>");
            let end = start
                + text[start..]
                    .find(&close)
                    .ok_or_else(|| format!("<{name}> has no end"))?
                + close.len();
            text.replace_range(
                start..end,
                &format!("<{name} unit='KiB'>{}</{name}>", boot / 1024),
            );
        }
    }
    if let Some(balloon) = original.and_then(balloon_element) {
        if !balloon.contains("model='none'") {
            if let Some(current) = balloon_element(&text) {
                text = text.replacen(&current, &balloon, 1);
            }
        }
    }
    Ok(text)
}

/// The `<memballoon>` element's whole text.
fn balloon_element(text: &str) -> Option<String> {
    let start = text.find("<memballoon")?;
    let tag_end = start + text[start..].find('>')?;
    let end = if text[..tag_end].ends_with('/') {
        tag_end + 1
    } else {
        start + text[start..].find("</memballoon>")? + "</memballoon>".len()
    };
    Some(text[start..end].to_owned())
}

/// Remove `start..end` with the indentation before it and the line break
/// after it, when it stands on lines of its own.
fn remove_lines(text: &mut String, start: usize, end: usize) {
    let line_start = text[..start].rfind('\n').map_or(0, |index| index + 1);
    let from = if text[line_start..start].trim().is_empty() {
        line_start
    } else {
        start
    };
    let to = if text[end..].starts_with('\n') {
        end + 1
    } else {
        end
    };
    text.replace_range(from..to, "");
}

/// The definition with its virtio-mem device holding `size` bytes, and
/// `<memory>` and `<maxMemory>` raised to fit it. A device's size changes
/// only when the VM next starts.
pub fn grow_device(xml: &str, size: u64) -> Result<String, String> {
    let inspection = inspect(xml)?;
    let old = inspection
        .device()
        .ok_or("the domain has no virtio-mem device")?
        .size_bytes;
    let total = inspection.memory_bytes.saturating_sub(old) + size;
    let mut text = xml.to_owned();
    let start = text
        .find("<memory model='virtio-mem'")
        .ok_or("can't find the virtio-mem device's text")?;
    let end = start
        + text[start..]
            .find("</memory>")
            .ok_or("the virtio-mem device has no end")?;
    let device = &text[start..end];
    let open = device.find("<size").ok_or("no <size> in the device")?;
    let close = open
        + device[open..]
            .find("</size>")
            .ok_or("no </size> in the device")?
        + "</size>".len();
    let grown = format!(
        "{}<size unit='KiB'>{}</size>{}",
        &device[..open],
        size / 1024,
        &device[close..]
    );
    text.replace_range(start..end, &grown);
    raise_max_memory(&mut text, total)?;
    if let Some(start) = find_element(&text, "memory") {
        let end = start
            + text[start..]
                .find("</memory>")
                .ok_or("<memory> has no end")?
            + "</memory>".len();
        text.replace_range(
            start..end,
            &format!("<memory unit='KiB'>{}</memory>", total / 1024),
        );
    }
    Ok(text)
}

/// `<maxMemory>` raised to `total` when it holds less: libvirt refuses a
/// definition whose memory comes to more than its `<maxMemory>`.
fn raise_max_memory(text: &mut String, total: u64) -> Result<(), String> {
    let Some(start) = find_element(text, "maxMemory") else {
        return Ok(());
    };
    let tag_end = start + text[start..].find('>').ok_or("<maxMemory> has no end")?;
    let end = start
        + text[start..]
            .find("</maxMemory>")
            .ok_or("<maxMemory> has no end")?
        + "</maxMemory>".len();
    let tag = &text[start..tag_end];
    let quoted = |key: &str| {
        let at = tag.find(&format!("{key}='"))? + key.len() + 2;
        Some(tag[at..at + tag[at..].find('\'')?].to_owned())
    };
    let value = text[tag_end + 1..end - "</maxMemory>".len()].trim();
    let current = libvirt_bytes(value, quoted("unit").as_deref())?;
    if current < total {
        let slots = quoted("slots").unwrap_or_else(|| "1".to_owned());
        text.replace_range(
            start..end,
            &format!(
                "<maxMemory slots='{slots}' unit='KiB'>{}</maxMemory>",
                total / 1024
            ),
        );
    }
    Ok(())
}

/// `<maxMemory>`, on its own line before the domain's `<memory>`.
fn insert_max_memory(text: &mut String, slots: usize, total: u64) -> Result<(), String> {
    let at = text
        .find("<memory unit")
        .or_else(|| text.find("<memory>"))
        .ok_or("the domain has no <memory>")?;
    let line_start = text[..at].rfind('\n').map_or(at, |index| index + 1);
    let indent = text[line_start..at].to_owned();
    let (insert_at, indent) = if indent.trim().is_empty() {
        (line_start, indent)
    } else {
        (at, String::new())
    };
    text.insert_str(
        insert_at,
        &format!(
            "{indent}<maxMemory slots='{slots}' unit='KiB'>{}</maxMemory>\n",
            total / 1024
        ),
    );
    Ok(())
}

/// The `<memballoon>` element, with whatever it holds, becomes
/// `<memballoon model='none'/>`.
fn turn_off_balloon(text: &mut String) -> Result<(), String> {
    let start = text
        .find("<memballoon")
        .ok_or("can't find the memory balloon's text")?;
    let tag_end = start
        + text[start..]
            .find('>')
            .ok_or("the memory balloon has no end")?;
    let end = if text[..tag_end].ends_with('/') {
        tag_end + 1
    } else {
        start
            + text[start..]
                .find("</memballoon>")
                .ok_or("the memory balloon has no end")?
            + "</memballoon>".len()
    };
    text.replace_range(start..end, "<memballoon model='none'/>");
    Ok(())
}

fn insert_device(text: &mut String, device: &str) -> Result<(), String> {
    let end = text
        .rfind("</devices>")
        .ok_or("the domain has no </devices>")?;
    let line_start = text[..end].rfind('\n').map_or(end, |index| index + 1);
    text.insert_str(line_start, device);
    Ok(())
}

/// Suggested settings for a domain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggested {
    pub min_ram: u64,
    pub max_ram: u64,
}

/// `min_ram` is the boot RAM; `max_ram` the lower of the device ceiling
/// and, for Linux, about four times boot RAM.
pub fn suggest(inspection: &Inspection, linux: bool) -> Option<Suggested> {
    let device = inspection.device()?;
    let block = device.block_bytes.max(2 * MIB);
    let boot = inspection.boot_bytes / block * block;
    let ceiling = boot + device.size_bytes;
    let max_ram = if linux {
        ceiling.min(boot * 4)
    } else {
        ceiling
    };
    Some(Suggested {
        min_ram: boot,
        max_ram: max_ram / block * block,
    })
}

/// How a definition holds memory for `min_ram` and `max_ram`: the VM
/// boots with its minimum, and its virtio-mem device holds the rest up to
/// its maximum. Both are whole device blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fitted {
    pub boot: u64,
    pub device: u64,
}

/// [`Fitted`] for these limits on a device with blocks of `block` bytes.
pub fn fitted(min_ram: u64, max_ram: u64, block: u64) -> Result<Fitted, String> {
    let block = block.max(2 * MIB);
    let boot = min_ram.div_ceil(block) * block;
    if max_ram <= boot {
        return Err(format!(
            "maximum RAM {} must be more than minimum RAM {}, or there is nothing to grow into",
            format_byte_size(max_ram),
            format_byte_size(min_ram)
        ));
    }
    Ok(Fitted {
        boot,
        device: (max_ram - boot).div_ceil(block) * block,
    })
}

impl Inspection {
    /// How this definition holds memory now, when it has one NUMA cell
    /// and a device: the only shape [`fit_memory`] changes.
    pub fn holds(&self) -> Option<Fitted> {
        let device = self.device()?;
        (self.numa_cells == 1).then_some(Fitted {
            boot: self.boot_bytes,
            device: device.size_bytes,
        })
    }
}

/// The definition with the VM booting with `min_ram` and its device
/// holding the rest up to `max_ram`: its NUMA cell, its device's size,
/// `<memory>`, `<currentMemory>` and, when it is too small, `<maxMemory>`.
/// It needs one NUMA cell and a device, as [`fix_xml`] leaves it. Like
/// every change to a definition, it takes effect when the VM next starts.
pub fn fit_memory(xml: &str, min_ram: u64, max_ram: u64) -> Result<String, String> {
    let inspection = inspect(xml)?;
    let device = inspection
        .device()
        .ok_or("the domain has no virtio-mem device")?;
    if inspection.numa_cells != 1 {
        return Err(format!(
            "its definition has {} NUMA cells; RAMpire sizes only one",
            inspection.numa_cells
        ));
    }
    let fit = fitted(min_ram, max_ram, device.block_bytes)?;
    let mut text = xml.to_owned();
    // The cell: what the VM boots with.
    let cell = find_element(&text, "cell").ok_or("the NUMA cell has no text")?;
    let cell_end = cell + text[cell..].find('>').ok_or("the NUMA cell has no end")?;
    let mut tag = text[cell..cell_end].to_owned();
    for (key, value) in [
        ("memory", (fit.boot / 1024).to_string()),
        ("unit", "KiB".to_owned()),
    ] {
        let at = tag
            .find(&format!(" {key}='"))
            .map(|at| at + key.len() + 3)
            .ok_or_else(|| format!("the NUMA cell has no {key}"))?;
        let end = at
            + tag[at..]
                .find('\'')
                .ok_or("the NUMA cell's text is cut short")?;
        tag.replace_range(at..end, &value);
    }
    text.replace_range(cell..cell_end, &tag);
    // The device, and the totals that hold it: <memory> as boot RAM, the
    // device and any other memory devices, <maxMemory> at least that.
    // grow_device counts the boot RAM as it was, so a bigger minimum is
    // held here: libvirt refuses a <maxMemory> below it.
    let others = inspection
        .memory_bytes
        .saturating_sub(inspection.boot_bytes + device.size_bytes);
    let mut text = grow_device(&text, fit.device)?;
    let total = fit.boot + fit.device + others;
    raise_max_memory(&mut text, total)?;
    if let Some(start) = find_element(&text, "memory") {
        let end = start
            + text[start..]
                .find("</memory>")
                .ok_or("<memory> has no end")?
            + "</memory>".len();
        text.replace_range(
            start..end,
            &format!("<memory unit='KiB'>{}</memory>", total / 1024),
        );
    }
    if let Some(start) = find_element(&text, "currentMemory") {
        let end = start
            + text[start..]
                .find("</currentMemory>")
                .ok_or("<currentMemory> has no end")?
            + "</currentMemory>".len();
        text.replace_range(
            start..end,
            &format!("<currentMemory unit='KiB'>{}</currentMemory>", total / 1024),
        );
    }
    Ok(text)
}

/// A review answer `vm add` read from the definition, or `None` when only
/// a person can give it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detected {
    pub question: &'static str,
    pub answer: Option<(bool, AnswerSource)>,
}

/// The yes-or-no review answers: detected where the definition shows
/// them, left to a person otherwise.
pub fn review_answers(inspection: &Inspection, host_has_rdma: bool) -> Vec<Detected> {
    let seen = |clear: bool| clear.then_some((true, AnswerSource::Detected));
    vec![
        Detected {
            question: "workload_reviewed",
            answer: None,
        },
        Detected {
            question: "no_vdpa",
            answer: Some((!inspection.vdpa, AnswerSource::Detected)),
        },
        Detected {
            question: "no_rdma_migration",
            // RDMA migration needs RDMA hardware: a host with none can't do it.
            answer: seen(!host_has_rdma),
        },
        Detected {
            question: "no_vfio_nvme",
            answer: seen(inspection.hostdevs == 0),
        },
        Detected {
            question: "no_mlock",
            answer: Some((!inspection.locked_memory, AnswerSource::Detected)),
        },
        Detected {
            question: "no_secure_virtualization",
            answer: Some((!inspection.launch_security, AnswerSource::Detected)),
        },
        Detected {
            question: "no_unsupported_vhost_user",
            answer: seen(!inspection.vhost_user),
        },
        Detected {
            question: "balloon_resize_inactive",
            answer: seen(
                inspection
                    .balloon
                    .as_deref()
                    .is_none_or(|model| model == "none"),
            ),
        },
    ]
}

/// What each review question asks a person.
pub fn question_text(question: &str) -> &'static str {
    match question {
        "workload_reviewed" => {
            "Can this VM's programs run with memory that grows and shrinks (none sized once at start-up or locking its memory)?"
        }
        "no_rdma_migration" => "Is this VM never live-migrated over RDMA?",
        "no_vfio_nvme" => {
            "Is none of the VM's passed-through devices an NVMe drive or one that pins memory?"
        }
        "no_unsupported_vhost_user" => {
            "Do all the VM's vhost-user devices support adding and removing memory?"
        }
        "balloon_resize_inactive" => "Is the VM's memory balloon never used to resize it?",
        _ => "Is this true for the VM?",
    }
}

/// What a review answer that no longer holds means for the VM.
pub fn unsafe_text(question: &str) -> &'static str {
    match question {
        "no_vdpa" => "it now has a vDPA device",
        "no_mlock" => "it now locks its memory",
        "no_secure_virtualization" => "it now uses secure virtualization (SEV, TDX)",
        "no_rdma_migration" => "it may be live-migrated over RDMA",
        "no_vfio_nvme" => "a passed-through device pins its memory",
        "no_unsupported_vhost_user" => "a vhost-user device can't follow memory changes",
        "balloon_resize_inactive" => "its memory balloon may resize it",
        _ => "an answer in its review no longer holds",
    }
}

/// Whether the host has RDMA hardware, which RDMA live migration needs:
/// any device under `/sys/class/infiniband`.
pub fn host_has_rdma() -> bool {
    std::fs::read_dir("/sys/class/infiniband").is_ok_and(|mut entries| entries.next().is_some())
}

/// The guest's kernel release from the QEMU guest agent's
/// `guest-get-osinfo` reply, when it has one.
pub fn kernel_release_from_osinfo(reply: &str) -> Option<String> {
    let reply: serde_json::Value = serde_json::from_str(reply).ok()?;
    let release = reply.get("return")?.get("kernel-release")?.as_str()?.trim();
    (!release.is_empty()).then(|| release.to_owned())
}

/// The review profile's text for these answers. `driver` is the guest's
/// driver version and where it came from.
pub fn review_toml(
    vm: &str,
    answers: &[(&str, bool, AnswerSource)],
    slots: u32,
    driver: (&str, AnswerSource),
) -> String {
    let source = |source: AnswerSource| match source {
        AnswerSource::Detected => "detected",
        AnswerSource::Confirmed => "confirmed",
    };
    let mut text = format!(
        "# The review of {vm}, made by `rampire vm add`. \"detected\" answers\n# were read from the VM's definition; \"confirmed\" ones were given by a\n# person.\nversion = 1\nguest_os = \"linux\"\n"
    );
    for (question, value, from) in answers {
        text.push_str(&format!(
            "{question} = {{ value = {value}, source = \"{}\" }}\n",
            source(*from)
        ));
    }
    text.push_str(&format!(
        "memory_slot_budget = {{ value = {slots}, source = \"detected\" }}\nvfio_mapping_budget = {{ value = 1, source = \"detected\" }}\nguest_driver_version = {{ value = \"{}\", source = \"{}\" }}\n",
        driver.0,
        source(driver.1)
    ));
    text
}

/// The VM's settings file.
pub fn vm_toml(alias: &str, priority: u32, review: &str, suggested: &Suggested) -> String {
    format!(
        "# Added by `rampire vm add`. Change a value with `rampire set VM KEY=VALUE`.\nalias = \"{alias}\"\npriority = {priority}\nreview = \"{review}\"\nguest_os = \"linux\"\n# The smallest the VM will be: it starts with this.\nmin_ram = \"{}\"\n# The biggest it may grow.\nmax_ram = \"{}\"\n",
        format_byte_size(suggested.min_ram),
        format_byte_size(suggested.max_ram),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANAGED: &str = "<domain type='kvm'>
  <name>web</name>
  <maxMemory slots='1' unit='KiB'>9437184</maxMemory>
  <memory unit='KiB'>9437184</memory>
  <cpu mode='host-passthrough'>
    <numa>
      <cell id='0' cpus='0-1' memory='1048576' unit='KiB'/>
    </numa>
  </cpu>
  <devices>
    <interface type='bridge'/>
    <channel type='unix'>
      <target type='virtio' name='org.rampire.control.0'/>
    </channel>
    <channel type='unix'>
      <target type='virtio' name='org.qemu.guest_agent.0'/>
    </channel>
    <memballoon model='none'/>
    <memory model='virtio-mem'>
      <target>
        <size unit='KiB'>8388608</size>
        <node>0</node>
        <block unit='KiB'>2048</block>
        <requested unit='KiB'>2097152</requested>
      </target>
      <alias name='ua-virtiomem0'/>
    </memory>
  </devices>
</domain>
";

    const PLAIN: &str = "<domain type='kvm'>
  <name>desk</name>
  <maxMemory slots='2' unit='KiB'>8388608</maxMemory>
  <memory unit='KiB'>2097152</memory>
  <cpu><numa><cell id='0' cpus='0' memory='2097152' unit='KiB'/></numa></cpu>
  <devices>
    <hostdev mode='subsystem' type='pci'/>
    <memballoon model='virtio'/>
  </devices>
</domain>
";

    #[test]
    fn reads_the_device_the_channel_and_the_boot_ram() {
        let web = inspect(MANAGED).unwrap();
        assert_eq!(web.boot_bytes, GIB);
        assert_eq!(web.max_memory_slots, Some(1));
        let device = web.device().unwrap();
        assert_eq!(device.alias.as_deref(), Some("ua-virtiomem0"));
        assert_eq!(
            (
                device.size_bytes,
                device.block_bytes,
                device.requested_bytes
            ),
            (8 * GIB, 2 * MIB, 2 * GIB)
        );
        assert!(web.control_channel);
        assert!(web.problems().is_empty(), "{:?}", web.problems());
        let desk = inspect(PLAIN).unwrap();
        assert!(desk.device().is_none() && !desk.control_channel);
        assert_eq!(desk.hostdevs, 1);
        // A USB device, a TV tuner say, pins no memory: nothing to ask.
        let tuner = PLAIN.replace(
            "<hostdev mode='subsystem' type='pci'/>",
            "<hostdev mode='subsystem' type='usb' managed='yes'><source><vendor id='0x2040'/></source></hostdev>",
        );
        assert_eq!(inspect(&tuner).unwrap().hostdevs, 0);
        assert_eq!(desk.balloon.as_deref(), Some("virtio"));
    }

    #[test]
    fn a_numa_cell_goes_in_cpu_not_in_cputune() {
        let tuned = "<domain type='kvm'>
  <memory unit='KiB'>2097152</memory>
  <vcpu placement='static'>2</vcpu>
  <cputune>
    <vcpupin vcpu='0' cpuset='1'/>
  </cputune>
  <cpu mode='host-passthrough' check='none'>
    <topology sockets='1' cores='2' threads='1'/>
  </cpu>
  <devices>
  </devices>
</domain>
";
        let inspection = inspect(tuned).unwrap();
        let text = fix_xml(tuned, &inspection).unwrap().unwrap();
        assert!(
            text.contains("<topology sockets='1' cores='2' threads='1'/>\n    <numa>"),
            "{text}"
        );
        assert!(text.contains("<cputune>\n    <vcpupin"), "{text}");
        let fixed = inspect(&text).unwrap();
        assert_eq!(fixed.numa_cells, 1);
        assert!(fixed.problems().is_empty(), "{:?}", fixed.problems());
    }

    #[test]
    fn a_grown_device_fits_in_max_memory_and_memory() {
        let web = inspect(MANAGED).unwrap();
        let old = web.device().unwrap().size_bytes;
        let grown = inspect(&grow_device(MANAGED, old + 4 * GIB).unwrap()).unwrap();
        assert_eq!(grown.device().unwrap().size_bytes, old + 4 * GIB);
        assert_eq!(grown.memory_bytes, web.memory_bytes + 4 * GIB);
        assert!(grown.problems().is_empty(), "{:?}", grown.problems());
        assert!(grow_device(PLAIN, GIB).is_err(), "no device to grow");
    }

    #[test]
    fn a_plain_definition_has_nothing_of_rampires_and_its_boot_ram() {
        // What vm add makes of a plain domain, back to what it was.
        let desk = inspect(PLAIN).unwrap();
        let added = fix_xml(PLAIN, &desk).unwrap().unwrap();
        let plain = plain_xml(&added, Some(PLAIN)).unwrap();
        for gone in ["virtio-mem", "org.rampire.control.0", "<maxMemory", "<numa"] {
            assert!(!plain.contains(gone), "{gone}:\n{plain}");
        }
        let back = inspect(&plain).unwrap();
        assert!(back.device().is_none() && !back.control_channel);
        assert_eq!(back.memory_bytes, desk.boot());
        assert_eq!(back.balloon, desk.balloon, "the balloon it had");
        assert!(back.guest_agent_channel, "the guest agent's channel stays");
        // A managed VM's own definition loses its device too.
        let managed = plain_xml(MANAGED, None).unwrap();
        assert!(
            !managed.contains("virtio-mem") && !managed.contains("<maxMemory"),
            "{managed}"
        );
        assert_eq!(inspect(&managed).unwrap().memory_bytes, 1024 * MIB);
    }

    #[test]
    fn a_restart_is_taken_by_every_change_but_saving_the_requested_size() {
        let ballooned = "<domain>\n  <memory unit='KiB'>4194304</memory>\n  <vcpu>2</vcpu>\n  <devices>\n    <memballoon model='virtio'/>\n  </devices>\n</domain>\n";
        for xml in [MANAGED, PLAIN, ballooned] {
            let inspection = inspect(xml).unwrap();
            let changes = inspection.fixable();
            assert_eq!(
                inspection.fixes_take_a_restart(),
                changes.iter().any(|change| !change.starts_with("save")),
                "{changes:?}"
            );
        }
        let fixed = fix_xml(MANAGED, &inspect(MANAGED).unwrap())
            .unwrap()
            .unwrap();
        assert!(!inspect(&fixed).unwrap().fixes_take_a_restart());
    }

    #[test]
    fn fixes_a_saved_request_and_adds_what_is_missing() {
        let web = inspect(MANAGED).unwrap();
        let fixed = fix_xml(MANAGED, &web).unwrap().expect("a fix");
        assert!(fixed.contains("<requested unit='KiB'>0</requested>"));
        assert_eq!(fixed.lines().count(), MANAGED.lines().count());
        let refixed = inspect(&fixed).unwrap();
        assert_eq!(
            fix_xml(&fixed, &refixed).unwrap(),
            None,
            "fixing is idempotent"
        );

        let desk = inspect(PLAIN).unwrap();
        assert_eq!(desk.fixable().len(), 4, "{:?}", desk.fixable());
        let fixed = inspect(&fix_xml(PLAIN, &desk).unwrap().unwrap()).unwrap();
        assert!(fixed.control_channel && fixed.guest_agent_channel);
        // The balloon is off, so its review question answers itself.
        assert_eq!(fixed.balloon.as_deref(), Some("none"));
        assert!(review_answers(&fixed, false)
            .iter()
            .any(|a| a.question == "balloon_resize_inactive" && a.answer.is_some()));
        let device = fixed.device().expect("a device was added");
        assert_eq!(device.size_bytes, 6 * GIB);
        assert_eq!(device.requested_bytes, 0);
        assert!(fixed.problems().is_empty(), "{:?}", fixed.problems());
    }

    #[test]
    fn a_domain_without_numa_or_slots_gets_them() {
        let bare = "<domain type='kvm'>
  <name>plain</name>
  <memory unit='KiB'>2097152</memory>
  <vcpu placement='static'>2</vcpu>
  <cpu mode='host-passthrough' check='none'/>
  <devices>
    <memballoon model='none'/>
  </devices>
</domain>
";
        let inspection = inspect(bare).unwrap();
        assert_eq!((inspection.vcpus, inspection.memory_bytes), (2, 2 * GIB));
        let problems: Vec<_> = inspection
            .problems()
            .into_iter()
            .map(|(problem, _)| problem)
            .collect();
        assert!(problems.iter().any(|p| p.contains("NUMA")));
        assert!(problems.iter().any(|p| p.contains("maxMemory")));
        assert_eq!(inspection.fixable().len(), 5, "{:?}", inspection.fixable());
        let text = fix_xml(bare, &inspection).unwrap().unwrap();
        assert!(
            text.contains("<cpu mode='host-passthrough' check='none'>\n    <numa>"),
            "{text}"
        );
        assert!(
            text.contains("  <maxMemory slots='1' unit='KiB'>8388608</maxMemory>\n  <memory"),
            "{text}"
        );
        let fixed = inspect(&text).unwrap();
        assert_eq!((fixed.numa_cells, fixed.boot_bytes), (1, 2 * GIB));
        assert_eq!(
            fixed.device().map(|device| device.size_bytes),
            Some(6 * GIB)
        );
        assert!(fixed.control_channel);
        assert!(fixed.problems().is_empty(), "{:?}", fixed.problems());
        assert_eq!(
            fix_xml(&text, &fixed).unwrap(),
            None,
            "fixing is idempotent"
        );
    }

    #[test]
    fn a_balloon_with_children_is_turned_off_whole() {
        let live = "<domain>\n  <devices>\n    <memballoon model='virtio'>\n      <stats period='5'/>\n      <alias name='balloon0'/>\n    </memballoon>\n  </devices>\n</domain>\n";
        let mut text = live.to_owned();
        turn_off_balloon(&mut text).unwrap();
        assert_eq!(
            text,
            "<domain>\n  <devices>\n    <memballoon model='none'/>\n  </devices>\n</domain>\n"
        );
    }

    #[test]
    fn turning_off_a_balloon_below_boot_ram_says_the_vm_gets_all_of_it() {
        let ballooned = "<domain>\n  <memory unit='KiB'>4194304</memory>\n  <currentMemory unit='KiB'>3145728</currentMemory>\n  <vcpu>2</vcpu>\n  <devices>\n    <memballoon model='virtio'/>\n  </devices>\n</domain>\n";
        let inspection = inspect(ballooned).unwrap();
        assert_eq!(inspection.current_memory_bytes, Some(3072 * MIB));
        let balloon = inspection
            .fixable()
            .into_iter()
            .find(|change| change.starts_with("turn off the memory balloon"))
            .unwrap();
        assert!(
            balloon.ends_with(
                "The VM then uses all 4096 MiB of its boot RAM, not the balloon's 3072 MiB"
            ),
            "{balloon}"
        );
        // With no target below boot RAM there is nothing more to say.
        let full = ballooned.replace("3145728", "4194304");
        let balloon = inspect(&full).unwrap().fixable().pop().unwrap();
        assert!(!balloon.contains("holds the VM"), "{balloon}");
    }

    #[test]
    fn a_domain_with_no_cpu_element_gets_one() {
        let bare = "<domain>\n  <memory unit='KiB'>1048576</memory>\n  <vcpu>1</vcpu>\n  <devices>\n  </devices>\n</domain>\n";
        let inspection = inspect(bare).unwrap();
        let fixed = inspect(&fix_xml(bare, &inspection).unwrap().unwrap()).unwrap();
        assert_eq!(fixed.numa_cells, 1);
        assert!(fixed.problems().is_empty(), "{:?}", fixed.problems());
    }

    #[test]
    fn a_domain_whose_slots_are_all_used_is_reported_not_edited() {
        let full = "<domain>\n  <maxMemory slots='1' unit='KiB'>4194304</maxMemory>\n  <memory unit='KiB'>2097152</memory>\n  <vcpu>1</vcpu>\n  <cpu><numa><cell id='0' cpus='0' memory='1048576' unit='KiB'/></numa></cpu>\n  <devices>\n    <memory model='dimm'><target><size unit='KiB'>1048576</size><node>0</node></target></memory>\n  </devices>\n</domain>\n";
        let inspection = inspect(full).unwrap();
        // Only the channel can be added: there is no free slot for a device.
        let fixed = inspect(&fix_xml(full, &inspection).unwrap().unwrap()).unwrap();
        assert!(fixed.control_channel && fixed.device().is_none());
    }

    #[test]
    fn suggests_settings_and_detects_review_answers() {
        let web = inspect(MANAGED).unwrap();
        let linux = suggest(&web, true).unwrap();
        assert_eq!((linux.min_ram, linux.max_ram), (GIB, 4 * GIB));
        assert_eq!(suggest(&web, false).unwrap().max_ram, 9 * GIB);
        let unanswered = |answers: Vec<Detected>| -> Vec<&'static str> {
            answers
                .iter()
                .filter(|answer| answer.answer.is_none())
                .map(|answer| answer.question)
                .collect()
        };
        // A host without RDMA hardware can't migrate over RDMA: only the
        // workload needs a person.
        assert_eq!(
            unanswered(review_answers(&web, false)),
            ["workload_reviewed"]
        );
        assert_eq!(
            unanswered(review_answers(&web, true)),
            ["workload_reviewed", "no_rdma_migration"]
        );
        let desk = review_answers(&inspect(PLAIN).unwrap(), false);
        let open: Vec<_> = desk
            .iter()
            .filter(|a| a.answer.is_none())
            .map(|a| a.question)
            .collect();
        assert!(open.contains(&"no_vfio_nvme") && open.contains(&"balloon_resize_inactive"));
    }

    #[test]
    fn the_definition_is_fitted_to_the_minimum_and_maximum() {
        // MANAGED boots with 1 GiB and has an 8 GiB device. As torrent101
        // was given on 2026-11-16: 512MB to 3GB. It boots with 512MB and
        // its device holds the other 2.5GB.
        let fitted_xml = fit_memory(MANAGED, 512 * MIB, 3 * GIB).unwrap();
        let after = inspect(&fitted_xml).unwrap();
        assert_eq!(
            after.holds(),
            Some(Fitted {
                boot: 512 * MIB,
                device: 2560 * MIB
            })
        );
        assert_eq!(after.memory_bytes, 3 * GIB);
        assert!(after
            .current_memory_bytes
            .is_none_or(|bytes| bytes == 3 * GIB));
        // Everything else is as it was.
        assert_eq!(
            after.device().unwrap().alias,
            inspect(MANAGED).unwrap().device().unwrap().alias
        );
        assert_eq!(
            fit_memory(&fitted_xml, 512 * MIB, 3 * GIB).unwrap(),
            fitted_xml
        );
        // Bigger than the device was: <maxMemory> grows to hold it.
        let big = inspect(&fit_memory(MANAGED, 2 * GIB, 24 * GIB).unwrap()).unwrap();
        assert_eq!(
            big.holds(),
            Some(Fitted {
                boot: 2 * GIB,
                device: 22 * GIB
            })
        );
        assert_eq!(big.memory_bytes, 24 * GIB);
        assert!(big.max_memory_bytes >= Some(24 * GIB), "{big:?}");
        // A bigger minimum alone: as safe_vm's on snow101, which libvirt
        // refused when <maxMemory> held the old boot RAM and the device.
        let raised = inspect(&fit_memory(MANAGED, 4 * GIB, 12 * GIB).unwrap()).unwrap();
        assert_eq!(raised.memory_bytes, 12 * GIB);
        assert_eq!(raised.max_memory_bytes, Some(12 * GIB));
        // A maximum that leaves nothing to grow into is said.
        let error = fit_memory(MANAGED, 2 * GIB, 2 * GIB).unwrap_err();
        assert!(error.contains("must be more than minimum RAM"), "{error}");
        assert!(fitted(GIB, GIB / 2, 2 * MIB).is_err());
        // Sizes are whole blocks.
        assert_eq!(
            fitted(GIB + 1, 2 * GIB, 2 * MIB).unwrap(),
            Fitted {
                boot: GIB + 2 * MIB,
                device: GIB - 2 * MIB
            }
        );
    }

    #[test]
    fn the_written_files_load() {
        let web = inspect(MANAGED).unwrap();
        let suggested = suggest(&web, true).unwrap();
        let vm: rampire_core::settings::VmSettings =
            toml::from_str(&vm_toml("ua-virtiomem0", 100, "web", &suggested)).unwrap();
        assert_eq!(vm.max_ram, 4 * GIB);
        let answers: Vec<_> = review_answers(&web, false)
            .into_iter()
            .map(|a| {
                let (value, source) = a.answer.unwrap_or((true, AnswerSource::Confirmed));
                (a.question, value, source)
            })
            .collect();
        let text = review_toml("web", &answers, 1, ("linux 6.12", AnswerSource::Detected));
        let root = std::env::temp_dir().join(format!("rampire-vm-review-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("web.toml");
        std::fs::write(&path, &text).unwrap();
        let review = rampire_host::attestation::read_review(&path);
        std::fs::remove_dir_all(&root).unwrap();
        let review = review.expect("valid profile");
        assert_eq!(review.no_vdpa.source, AnswerSource::Detected);
        assert_eq!(review.workload_reviewed.source, AnswerSource::Confirmed);
        assert_eq!(review.guest_driver_version.source, AnswerSource::Detected);
    }

    #[test]
    fn the_kernel_release_is_read_from_the_guest_agent() {
        assert_eq!(
            kernel_release_from_osinfo(
                r#"{"return":{"name":"Debian GNU/Linux","kernel-release":"6.12.86+deb13-amd64"}}"#
            ),
            Some("6.12.86+deb13-amd64".to_owned())
        );
        for reply in [
            "",
            "{}",
            r#"{"return":{}}"#,
            r#"{"return":{"kernel-release":" "}}"#,
        ] {
            assert_eq!(kernel_release_from_osinfo(reply), None, "{reply}");
        }
    }
}
