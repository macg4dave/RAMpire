//! `rampire init`: suggest a host's global settings from its memory and its
//! libvirt domains.
//!
//! A domain with a virtio-mem device is one RAMpire will manage: only its
//! boot RAM, the memory of its NUMA cells, is set aside. Every other domain
//! keeps all its memory, so all of it is set aside. The pool is what is left
//! of the host's memory after the host's own reserve.

use rampire_core::GIB;

/// The memory one domain needs, read from its definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainMemory {
    pub name: String,
    /// Has a virtio-mem device, so RAMpire can manage it.
    pub resizable: bool,
    /// The domain's `<memory>`.
    pub memory_bytes: u64,
    /// The memory of its NUMA cells: the fixed boot RAM of a resizable VM.
    pub boot_bytes: u64,
}

impl DomainMemory {
    /// What the pool can never use for this domain.
    pub fn set_aside_bytes(&self) -> u64 {
        if self.resizable && self.boot_bytes > 0 {
            self.boot_bytes
        } else {
            self.memory_bytes
        }
    }
}

/// The suggested global settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub host_bytes: u64,
    pub host_reserve_bytes: u64,
    /// `None` when nothing is left for a pool.
    pub pool_bytes: Option<u64>,
}

/// The host keeps an eighth of its memory, between 2 and 8 GiB, in whole
/// GiB. The pool is the rest after every domain's set-aside memory, rounded
/// down to whole GiB.
pub fn suggest(host_bytes: u64, domains: &[DomainMemory]) -> Suggestion {
    let reserve = (host_bytes / 8).clamp(2 * GIB, 8 * GIB) / GIB * GIB;
    let set_aside = domains
        .iter()
        .map(DomainMemory::set_aside_bytes)
        .fold(0, u64::saturating_add);
    let pool = host_bytes.saturating_sub(reserve).saturating_sub(set_aside) / GIB * GIB;
    Suggestion {
        host_bytes,
        host_reserve_bytes: reserve,
        pool_bytes: (pool > 0).then_some(pool),
    }
}

/// `global.toml` for a suggestion, with notes saying where each value came
/// from.
pub fn global_toml(suggestion: &Suggestion, domains: &[DomainMemory]) -> String {
    let gib = |bytes: u64| bytes / GIB;
    let mut text = String::from(
        "# Written by `rampire init` from this host's memory and its VMs.\n\
         # Change a value with `rampire set global KEY=VALUE`, or edit this\n\
         # file and run `rampire apply`.\n\n",
    );
    // To a tenth: a host's memory is rarely whole GiB, and rounding down
    // would say a 1.9 GiB host has 1 GiB.
    let tenths = |bytes: u64| bytes as f64 / GIB as f64;
    let eighth = suggestion.host_bytes / 8;
    let (bound, word) = if eighth < 2 * GIB {
        (Some("the least it keeps"), "since")
    } else if eighth > 8 * GIB {
        (Some("the most it keeps"), "though")
    } else {
        (None, "")
    };
    let reserve_why = match bound {
        Some(bound) => format!(
            "{bound}, {word} an\n# eighth of its {:.1} GiB would be {:.1} GiB.",
            tenths(suggestion.host_bytes),
            tenths(eighth)
        ),
        None => format!(
            "an eighth of its {:.1} GiB, in\n# whole GiB.",
            tenths(suggestion.host_bytes)
        ),
    };
    text.push_str(&format!(
        "# Memory the host keeps for itself: {reserve} GiB, {reserve_why}\nhost_reserve = \"{reserve}GB\"\n\n",
        reserve = gib(suggestion.host_reserve_bytes),
    ));
    text.push_str(if domains.is_empty() {
        "# The pool every managed VM grows from: the host's memory, less the\n# reserve. The host has no VMs yet.\n"
    } else {
        "# The pool every managed VM grows from: the host's memory, less the\n# reserve, less what each of these VMs needs whatever happens:\n"
    });
    for domain in domains {
        let what = if domain.resizable && domain.boot_bytes > 0 {
            "boot RAM; it can be managed"
        } else {
            "all of it; it has no virtio-mem device"
        };
        text.push_str(&format!(
            "#   {}: {} MiB ({what})\n",
            domain.name,
            domain.set_aside_bytes() >> 20
        ));
    }
    match suggestion.pool_bytes {
        Some(pool) => text.push_str(&format!("pool = \"{}GB\"\n", gib(pool))),
        None => text.push_str("# Nothing is left for a pool on this host.\n"),
    }
    text
}

/// Read a domain's memory from its XML definition, as `vm add` reads it.
pub fn parse_domain(name: &str, xml: &str) -> Result<DomainMemory, String> {
    let inspection = crate::vm::inspect(xml).map_err(|error| format!("{name}: {error}"))?;
    if inspection.memory_bytes == 0 {
        return Err(format!("{name}: domain XML has no <memory>"));
    }
    Ok(DomainMemory {
        name: name.to_owned(),
        resizable: !inspection.devices.is_empty(),
        memory_bytes: inspection.memory_bytes,
        boot_bytes: inspection.boot_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const RESIZABLE: &str = "<domain type='kvm'><name>web</name>\
        <maxMemory slots='1' unit='KiB'>17825792</maxMemory>\
        <memory unit='KiB'>9437184</memory>\
        <cpu><numa><cell id='0' cpus='0-1' memory='1048576' unit='KiB'/></numa></cpu>\
        <devices><memory model='virtio-mem'><target><size unit='GiB'>8</size>\
        <node>0</node><block unit='KiB'>2048</block><requested unit='KiB'>0</requested>\
        </target><alias name='ua-virtiomem0'/></memory></devices></domain>";
    const FIXED: &str = "<domain type='kvm'><name>desk</name><memory>4194304</memory>\
        <devices><disk type='file'/></devices></domain>";

    #[test]
    fn a_virtio_mem_domain_sets_aside_its_boot_ram_and_another_all_its_memory() {
        let web = parse_domain("web", RESIZABLE).unwrap();
        assert!(web.resizable);
        assert_eq!(web.memory_bytes, 9 * GIB);
        assert_eq!(web.set_aside_bytes(), GIB);
        let desk = parse_domain("desk", FIXED).unwrap();
        assert!(!desk.resizable);
        assert_eq!(desk.set_aside_bytes(), 4 * GIB);
        assert!(parse_domain("bad", "<domain/>").is_err());
    }

    #[test]
    fn the_pool_is_what_is_left_after_the_reserve_and_every_vm() {
        let domains = [
            parse_domain("web", RESIZABLE).unwrap(),
            parse_domain("desk", FIXED).unwrap(),
        ];
        let suggestion = suggest(64 * GIB, &domains);
        assert_eq!(suggestion.host_reserve_bytes, 8 * GIB);
        assert_eq!(suggestion.pool_bytes, Some(51 * GIB));
        assert_eq!(suggest(16 * GIB, &[]).host_reserve_bytes, 2 * GIB);
        assert_eq!(suggest(4 * GIB, &domains).pool_bytes, None);

        let text = global_toml(&suggestion, &domains);
        let globals: rampire_core::settings::GlobalSettings = toml::from_str(&text).unwrap();
        assert_eq!(globals.pool, Some(51 * GIB));
        assert_eq!(globals.host_reserve, 8 * GIB);
        assert!(
            text.contains("#   web: 1024 MiB (boot RAM; it can be managed)"),
            "{text}"
        );
    }

    #[test]
    fn the_notes_say_why_the_reserve_is_what_it_is_and_list_no_vms_when_there_are_none() {
        // A small host: an eighth would be under the 2 GiB the host keeps.
        let small = global_toml(&suggest(4 * GIB - 200 * (GIB >> 10), &[]), &[]);
        assert!(
            small.contains(
                "2 GiB, the least it keeps, since an\n# eighth of its 3.8 GiB would be 0.5 GiB."
            ),
            "{small}"
        );
        assert!(
            small.contains("reserve. The host has no VMs yet.\n"),
            "{small}"
        );
        assert!(!small.contains("whatever happens"), "{small}");
        // A large host: more than 8 GiB would be an eighth.
        let large = global_toml(&suggest(128 * GIB, &[]), &[]);
        assert!(
            large.contains("8 GiB, the most it keeps, though an"),
            "{large}"
        );
        // In between, an eighth is kept.
        let middle = global_toml(&suggest(32 * GIB, &[]), &[]);
        assert!(
            middle.contains("4 GiB, an eighth of its 32.0 GiB"),
            "{middle}"
        );
    }
}
