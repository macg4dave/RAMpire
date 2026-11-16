//! What a VM's fingerprint leaves out: the details libvirt and QEMU change
//! on every start or while memory moves (allocation progress, domain ids,
//! pty paths, generated names, file descriptor numbers), stripped from the
//! domain XML and QEMU arguments before they are hashed. Everything else
//! in them is attested.

pub(super) fn scrub_allocation_state(xml: &str) -> String {
    let mut scrubbed = with_current(xml);
    // Libvirt derives the live top-level currentMemory value from base memory
    // plus the virtio-mem allocation. It therefore changes alongside the
    // alias-scoped requested/current fields and is not configuration drift.
    for element in ["currentMemory", "requested", "current"] {
        let mut offset = 0;
        while let Some(start) = scrubbed[offset..].find(&format!("<{element}")) {
            let start = offset + start;
            let name_end = start + element.len() + 1;
            if !scrubbed[name_end..]
                .chars()
                .next()
                .is_some_and(|character| character == '>' || character.is_whitespace())
            {
                offset = name_end;
                continue;
            }
            let Some(open_end) = scrubbed[start..].find('>') else {
                break;
            };
            let content_start = start + open_end + 1;
            let close = format!("</{element}>");
            let Some(close_start) = scrubbed[content_start..].find(&close) else {
                break;
            };
            let close_start = content_start + close_start;
            scrubbed.replace_range(content_start..close_start, "<allocation-state>");
            offset = content_start + "<allocation-state>".len() + close.len();
        }
    }
    scrubbed
}

/// `xml` with a `<current>` after each virtio-mem `<requested>` that has
/// none, laid out as libvirt writes it. libvirt shows `current` only once
/// QEMU has reported the device's size, so just after a VM starts it may be
/// missing; its value is scrubbed either way, and with this the VM's
/// fingerprint is the same before and after it appears.
fn with_current(xml: &str) -> String {
    let mut text = xml.to_owned();
    let mut offset = 0;
    while let Some(found) = text[offset..].find("<requested") {
        let start = offset + found;
        let Some(close) = text[start..].find("</requested>") else {
            break;
        };
        let end = start + close + "</requested>".len();
        offset = end;
        let after = text[end..].trim_start();
        if after.starts_with("<current") {
            continue;
        }
        let line_start = text[..start].rfind('\n').map_or(0, |index| index + 1);
        let indent = &text[line_start..start];
        let separator = if indent.trim().is_empty() {
            format!("\n{indent}")
        } else {
            String::new()
        };
        let inserted = format!("{separator}<current unit='KiB'>0</current>");
        text.insert_str(end, &inserted);
        offset = end + inserted.len();
    }
    text
}

/// Libvirt assigns a new domain `id` and, for `type='dynamic'` security
/// labels, a new random label every time the domain starts. Neither is
/// configuration, so a VM restart must not look like drift. Static labels
/// stay fingerprint inputs.
pub(super) fn scrub_runtime_identity(xml: &str) -> String {
    let mut scrubbed = xml.to_owned();
    if let Some(start) = scrubbed.find("<domain") {
        let tag_end = scrubbed[start..]
            .find('>')
            .map_or(scrubbed.len(), |end| start + end);
        for quote in ['\'', '"'] {
            let attribute = format!(" id={quote}");
            if let Some(id) = scrubbed[start..tag_end].find(&attribute) {
                let value_start = start + id + attribute.len();
                if let Some(value_end) = scrubbed[value_start..tag_end].find(quote) {
                    scrubbed.replace_range(start + id..value_start + value_end + 1, "");
                }
                break;
            }
        }
    }
    // Virtio-serial target connection state is runtime status. Preserve its
    // configured name, source socket and address, but not this status attribute.
    let mut channel_offset = 0;
    while let Some(found) = scrubbed[channel_offset..].find("<channel ") {
        let start = channel_offset + found;
        let Some(close) = scrubbed[start..].find("</channel>") else {
            break;
        };
        let end = start + close;
        let mut block = scrubbed[start..end].to_owned();
        if let Some(target) = block.find("<target ") {
            let target_end = block[target..]
                .find('>')
                .map_or(block.len(), |i| target + i);
            if block[target..target_end].contains("type='virtio'")
                || block[target..target_end].contains("type=\"virtio\"")
            {
                for quote in ['\'', '"'] {
                    let attribute = format!(" state={quote}");
                    if let Some(at) = block[target..target_end].find(&attribute) {
                        let at = target + at;
                        let value = at + attribute.len();
                        if let Some(close) = block[value..target_end].find(quote) {
                            block.replace_range(at..value + close + 1, "");
                        }
                        break;
                    }
                }
            }
        }
        scrubbed.replace_range(start..end, &block);
        channel_offset = start + block.len() + "</channel>".len();
    }
    scrubbed = scrub_pty_paths(&scrubbed);
    scrubbed = scrub_generated_interface_names(&scrubbed);
    scrubbed = scrub_channel_directory_ids(&scrubbed);
    let mut offset = 0;
    while let Some(start) = scrubbed[offset..].find("<seclabel") {
        let start = offset + start;
        let Some(end) = scrubbed[start..].find("</seclabel>") else {
            break;
        };
        let end = start + end;
        let open_end = scrubbed[start..end]
            .find('>')
            .map_or(end, |open| start + open);
        let tag = &scrubbed[start..open_end];
        if tag.contains("type='dynamic'") || tag.contains("type=\"dynamic\"") {
            let mut block = scrubbed[start..end].to_owned();
            for element in ["label", "imagelabel"] {
                let open = format!("<{element}>");
                let close = format!("</{element}>");
                if let (Some(content), Some(close_at)) = (block.find(&open), block.find(&close)) {
                    let content = content + open.len();
                    if content <= close_at {
                        block.replace_range(content..close_at, "<runtime-label>");
                    }
                }
            }
            scrubbed.replace_range(start..end, &block);
            offset = start + block.len();
        } else {
            offset = end;
        }
    }
    scrubbed
}

/// Libvirt allocates a new pseudo-terminal (`/dev/pts/N`) for every
/// `type='pty'` serial, console, parallel or channel device each time the
/// domain starts, and reports it as `tty=` and `<source path=>`. Only the
/// allocated path is blanked; the device, its type and its target stay
/// fingerprint inputs.
fn scrub_pty_paths(xml: &str) -> String {
    let mut scrubbed = xml.to_owned();
    for element in ["serial", "console", "parallel", "channel"] {
        let open = format!("<{element} ");
        let close = format!("</{element}>");
        let mut offset = 0;
        while let Some(found) = scrubbed[offset..].find(&open) {
            let start = offset + found;
            let Some(length) = scrubbed[start..].find(&close) else {
                break;
            };
            let end = start + length;
            let tag_end = scrubbed[start..end].find('>').map_or(end, |i| start + i);
            let tag = &scrubbed[start..tag_end];
            if !(tag.contains("type='pty'") || tag.contains("type=\"pty\"")) {
                offset = end + close.len();
                continue;
            }
            let mut block = scrubbed[start..end].to_owned();
            for quote in ['\'', '"'] {
                for attribute in ["tty", "path"] {
                    let prefix = format!(" {attribute}={quote}/dev/pts/");
                    let mut at = 0;
                    while let Some(found) = block[at..].find(&prefix) {
                        let value = at + found + prefix.len() - "/dev/pts/".len();
                        let Some(value_end) = block[value..].find(quote) else {
                            break;
                        };
                        block.replace_range(value..value + value_end, "<pty>");
                        at = value + "<pty>".len();
                    }
                }
            }
            scrubbed.replace_range(start..end, &block);
            offset = start + block.len() + close.len();
        }
    }
    scrubbed
}

/// Libvirt names an interface's host device `vnetN`, `macvtapN` or `macvlanN`
/// when the configuration gives none, choosing N afresh on each start. Those
/// prefixes are reserved for generated names, so a configured name is kept.
fn scrub_generated_interface_names(xml: &str) -> String {
    let mut scrubbed = xml.to_owned();
    for quote in ['\'', '"'] {
        let attribute = format!("<target dev={quote}");
        let mut offset = 0;
        while let Some(found) = scrubbed[offset..].find(&attribute) {
            let value = offset + found + attribute.len();
            let Some(length) = scrubbed[value..].find(quote) else {
                break;
            };
            let name = &scrubbed[value..value + length];
            let generated = ["vnet", "macvtap", "macvlan"].iter().any(|prefix| {
                name.strip_prefix(prefix)
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            });
            if generated {
                scrubbed.replace_range(value..value + length, "<generated>");
                offset = value + "<generated>".len();
            } else {
                offset = value + length;
            }
        }
    }
    scrubbed
}

/// Libvirt puts the sockets of channels with no configured path in a
/// directory named after the runtime domain id
/// (`/run/libvirt/qemu/channel/21-vm/`), which changes on each start.
fn scrub_channel_directory_ids(xml: &str) -> String {
    const PREFIX: &str = "/run/libvirt/qemu/channel/";
    let mut scrubbed = xml.to_owned();
    let mut offset = 0;
    while let Some(found) = scrubbed[offset..].find(PREFIX) {
        let id = offset + found + PREFIX.len();
        let digits = scrubbed[id..]
            .find(|character: char| !character.is_ascii_digit())
            .unwrap_or(scrubbed.len() - id);
        if digits > 0 && scrubbed[id + digits..].starts_with('-') {
            scrubbed.replace_range(id..id + digits, "#");
            offset = id + 1;
        } else {
            offset = id;
        }
    }
    scrubbed
}

/// Removes the `requested-size` member, value and one adjacent separator from
/// the generated argv. Libvirt omits the member entirely when the request is
/// zero, so blanking only its value would make a zero request look like
/// configuration drift.
pub(super) fn scrub_qemu_argv(arguments: &str) -> String {
    const KEY: &str = "requested-size";
    let mut scrubbed = arguments.to_owned();
    let mut offset = 0;
    while let Some(found) = scrubbed[offset..].find(KEY) {
        let key_start = offset + found;
        let key_end = key_start + KEY.len();
        let member_start = if scrubbed[..key_start].ends_with('"') {
            key_start - 1
        } else {
            key_start
        };
        let mut separator = key_end;
        while let Some(character) = scrubbed[separator..].chars().next() {
            if character == '"' || character.is_whitespace() {
                separator += character.len_utf8();
            } else {
                break;
            }
        }
        let Some(separator_character) = scrubbed[separator..].chars().next() else {
            break;
        };
        if !matches!(separator_character, '=' | ':') {
            offset = key_end;
            continue;
        }
        let mut value_start = separator + separator_character.len_utf8();
        while let Some(character) = scrubbed[value_start..].chars().next() {
            if character.is_whitespace() {
                value_start += character.len_utf8();
            } else {
                break;
            }
        }
        let quoted = scrubbed[value_start..].starts_with('"');
        if quoted {
            value_start += 1;
        }
        let mut member_end = scrubbed[value_start..]
            .find(|character: char| {
                (quoted && character == '"')
                    || (!quoted
                        && (character == ',' || character == '}' || character.is_whitespace()))
            })
            .map_or(scrubbed.len(), |end| value_start + end);
        if quoted && member_end < scrubbed.len() {
            member_end += 1;
        }
        let (remove_start, remove_end) = if scrubbed[member_end..].starts_with(',') {
            (member_start, member_end + 1)
        } else if scrubbed[..member_start].ends_with(',') {
            (member_start - 1, member_end)
        } else {
            (member_start, member_end)
        };
        scrubbed.replace_range(remove_start..remove_end, "");
        offset = remove_start;
    }
    scrubbed
}

/// Blanks the file-descriptor numbers libvirt puts in the generated argv
/// (`"fd":"38"`, `vhostfd=40`, `fds=a:b`). They are libvirtd's own open
/// descriptors at generation time and differ between two calls on an
/// unchanged domain.
pub(super) fn scrub_descriptor_numbers(arguments: &str) -> String {
    const KEYS: [&str; 4] = ["fd", "vhostfd", "fds", "vhostfds"];
    let mut scrubbed = String::with_capacity(arguments.len());
    let mut rest = arguments;
    'scan: while !rest.is_empty() {
        for key in KEYS {
            for (prefix, quoted) in [
                (format!("\"{key}\":\""), true),
                (format!("\"{key}\":"), false),
                (format!(",{key}="), false),
                (format!(" {key}="), false),
            ] {
                let Some(after) = rest.strip_prefix(prefix.as_str()) else {
                    continue;
                };
                let length = after
                    .find(|character: char| !(character.is_ascii_digit() || character == ':'))
                    .unwrap_or(after.len());
                let closed = !quoted || after[length..].starts_with('"');
                if length > 0 && closed {
                    scrubbed.push_str(&prefix);
                    scrubbed.push('#');
                    rest = &after[length..];
                    continue 'scan;
                }
            }
        }
        let Some(character) = rest.chars().next() else {
            break;
        };
        scrubbed.push(character);
        rest = &rest[character.len_utf8()..];
    }
    scrubbed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_device_without_its_current_size_yet_scrubs_as_one_with_it() {
        // libvirt's live XML just after a VM starts, and once QEMU has
        // reported the device's size.
        let before = "<domain>\n  <devices>\n    <memory model='virtio-mem'>\n      <target>\n        <size unit='KiB'>2097152</size>\n        <block unit='KiB'>2048</block>\n        <requested unit='KiB'>0</requested>\n      </target>\n    </memory>\n  </devices>\n</domain>\n";
        let after = before.replace(
            "<requested unit='KiB'>0</requested>\n",
            "<requested unit='KiB'>0</requested>\n        <current unit='KiB'>116736</current>\n",
        );
        assert_eq!(
            scrub_allocation_state(before),
            scrub_allocation_state(&after)
        );
        // One on a line of its own and one inline both get it once.
        let inline = "<target><requested unit='KiB'>0</requested></target>";
        assert_eq!(
            with_current(inline),
            "<target><requested unit='KiB'>0</requested><current unit='KiB'>0</current></target>"
        );
        assert_eq!(with_current(&with_current(inline)), with_current(inline));
    }
    use crate::attestation::sha256_hex;

    #[test]
    fn channel_connection_status_is_runtime_but_binding_is_attested() {
        let xml = "<domain><devices><channel type='unix'><source path='/wake'/><target type='virtio' name='org.rampire.wake.0' state='connected'/><address port='2'/></channel></devices></domain>";
        assert_eq!(
            scrub_runtime_identity(xml),
            scrub_runtime_identity(&xml.replace("connected", "disconnected"))
        );
        assert_eq!(
            scrub_runtime_identity(xml),
            scrub_runtime_identity(&xml.replace(" state='connected'", ""))
        );
        for changed in [
            xml.replace("/wake", "/other"),
            xml.replace("wake.0", "other.0"),
            xml.replace("port='2'", "port='3'"),
        ] {
            assert_ne!(
                scrub_runtime_identity(xml),
                scrub_runtime_identity(&changed)
            );
        }
    }

    #[test]
    fn allocation_progress_does_not_change_domain_fingerprint() {
        let before = "<domain><currentMemory>8</currentMemory><memory><target><requested unit='MiB'>2</requested><current unit='MiB'>0</current></target></memory></domain>";
        let after = "<domain><currentMemory>12</currentMemory><memory><target><requested unit='MiB'>4</requested><current unit='MiB'>4</current></target></memory></domain>";
        assert_eq!(
            sha256_hex(scrub_allocation_state(before).as_bytes()),
            sha256_hex(scrub_allocation_state(after).as_bytes())
        );
        assert_eq!(
            sha256_hex(scrub_qemu_argv("-device virtio-mem,requested-size=1G,node=0").as_bytes()),
            sha256_hex(scrub_qemu_argv("-device virtio-mem,requested-size=4G,node=0").as_bytes())
        );
        assert_eq!(
            sha256_hex(
                scrub_qemu_argv(
                    r#"-device '{"driver":"virtio-mem-pci","requested-size":1071644672,"node":0}'"#
                )
                .as_bytes()
            ),
            sha256_hex(
                scrub_qemu_argv(
                    r#"-device '{"driver":"virtio-mem-pci","requested-size":1073741824,"node":0}'"#
                )
                .as_bytes()
            )
        );
        // Libvirt omits the member when the request is zero.
        let zero = r#"-device '{"driver":"virtio-mem-pci","node":0,"memdev":"m0","id":"memory0"}'"#;
        for argv in [
            r#"-device '{"driver":"virtio-mem-pci","node":0,"requested-size":1073741824,"memdev":"m0","id":"memory0"}'"#,
            r#"-device '{"driver":"virtio-mem-pci","node":0,"memdev":"m0","id":"memory0","requested-size":2097152}'"#,
            r#"-device '{"driver":"virtio-mem-pci","node":0,"memdev":"m0","id":"memory0","requested-size":"4194304"}'"#,
        ] {
            assert_eq!(scrub_qemu_argv(argv), scrub_qemu_argv(zero), "{argv}");
        }
        assert_eq!(
            scrub_qemu_argv("-device virtio-mem,node=0,requested-size=1G,id=m"),
            scrub_qemu_argv("-device virtio-mem,node=0,id=m")
        );
        assert_eq!(
            scrub_allocation_state(before),
            scrub_allocation_state(after)
        );
        assert_eq!(
            scrub_descriptor_numbers(
                r#"-netdev '{"type":"tap","fd":"34","vhost":true,"vhostfd":"38","id":"n"}'"#
            ),
            scrub_descriptor_numbers(
                r#"-netdev '{"type":"tap","fd":"38","vhost":true,"vhostfd":"40","id":"n"}'"#
            )
        );
        assert_eq!(
            scrub_descriptor_numbers("-netdev tap,fds=3:4,vhostfds=5:6,id=n -m size=8"),
            "-netdev tap,fds=#,vhostfds=#,id=n -m size=8"
        );
        // Other members, and paths that merely contain the key, are kept.
        for argv in [
            r#"{"type":"tap","fd":"38","id":"n1"}"#,
            "-chardev socket,path=/run/fd=3",
        ] {
            assert_ne!(
                scrub_descriptor_numbers(argv),
                scrub_descriptor_numbers(&argv.replace('1', "2").replace("/run", "/var"))
            );
        }
    }

    #[test]
    fn domain_restart_identity_does_not_change_domain_fingerprint() {
        let started = |id: u32, label: &str| {
            format!(
                "<domain type='kvm' id='{id}'><name>vm</name><seclabel type='dynamic' model='selinux' relabel='yes'><label>system_u:system_r:svirt_t:s0:{label}</label><imagelabel>system_u:object_r:svirt_image_t:s0:{label}</imagelabel></seclabel><seclabel type='dynamic' model='dac' relabel='yes'><label>+107:+107</label><imagelabel>+107:+107</imagelabel></seclabel></domain>"
            )
        };
        assert_eq!(
            scrub_runtime_identity(&started(3, "c1,c2")),
            scrub_runtime_identity(&started(10, "c148,c988"))
        );
        assert!(!scrub_runtime_identity(&started(3, "c1,c2")).contains("id="));
        // Configuration around them is still covered.
        assert_ne!(
            scrub_runtime_identity(&started(3, "c1,c2")),
            scrub_runtime_identity(&started(3, "c1,c2").replace("<name>vm", "<name>other"))
        );
        let fixed = |label: &str| {
            format!("<domain id='1'><seclabel type='static' model='selinux'><label>{label}</label></seclabel></domain>")
        };
        assert_ne!(
            scrub_runtime_identity(&fixed("s0:c1")),
            scrub_runtime_identity(&fixed("s0:c2"))
        );
        // A device id elsewhere is not the domain id.
        assert!(
            scrub_runtime_identity("<domain id='1'><memory id='m'/></domain>").contains("id='m'")
        );
    }

    #[test]
    fn pty_allocation_does_not_change_domain_fingerprint() {
        let started = |pts: u32| {
            format!(
                "<domain id='1'><devices><serial type='pty'><source path='/dev/pts/{pts}'/><target type='isa-serial' port='0'><model name='isa-serial'/></target><alias name='serial0'/></serial><console type='pty' tty='/dev/pts/{pts}'><source path='/dev/pts/{pts}'/><target type='serial' port='0'/><alias name='serial0'/></console><serial type='file'><source path='/var/log/s{pts}.log'/></serial></devices></domain>"
            )
        };
        let pty_only = |xml: String| xml.replace("/var/log/s8.log", "/var/log/s7.log");
        assert_eq!(
            scrub_runtime_identity(&started(7)),
            scrub_runtime_identity(&pty_only(started(8)))
        );
        // A file-backed serial path is configuration, as is the target.
        assert_ne!(
            scrub_runtime_identity(&started(7)),
            scrub_runtime_identity(&started(8))
        );
        assert_ne!(
            scrub_runtime_identity(&started(7)),
            scrub_runtime_identity(&started(7).replace("port='0'", "port='1'"))
        );
    }

    #[test]
    fn generated_interface_and_channel_names_do_not_change_domain_fingerprint() {
        let started = |id: u32, tap: &str| {
            format!(
                "<domain id='{id}'><devices><interface type='bridge'><mac address='52:54:00:a6:60:01'/><source bridge='br0'/><target dev='{tap}'/></interface><channel type='unix'><source mode='bind' path='/run/libvirt/qemu/channel/{id}-vm/org.rampire.control.0'/><target type='virtio' name='org.rampire.control.0'/></channel></devices></domain>"
            )
        };
        assert_eq!(
            scrub_runtime_identity(&started(20, "vnet7")),
            scrub_runtime_identity(&started(21, "vnet232"))
        );
        assert_eq!(
            scrub_runtime_identity(&started(20, "macvtap0")),
            scrub_runtime_identity(&started(21, "macvtap3"))
        );
        // A configured device name, the VM name in the channel path and the
        // channel's own name stay fingerprint inputs.
        for changed in [
            started(20, "vnet7").replace("vnet7", "tap-safe"),
            started(20, "vnet7").replace("-vm/", "-other/"),
            started(20, "vnet7").replace("control.0'/>", "control.1'/>"),
        ] {
            assert_ne!(
                scrub_runtime_identity(&started(20, "vnet7")),
                scrub_runtime_identity(&changed)
            );
        }
        assert_ne!(
            scrub_runtime_identity(&started(20, "mytap1")),
            scrub_runtime_identity(&started(20, "mytap2"))
        );
    }

    #[test]
    fn similarly_prefixed_xml_elements_remain_fingerprint_inputs() {
        let before = "<domain><currentMemorySlots>8</currentMemorySlots></domain>";
        let after = "<domain><currentMemorySlots>16</currentMemorySlots></domain>";

        assert_ne!(
            scrub_allocation_state(before),
            scrub_allocation_state(after)
        );
    }
}
