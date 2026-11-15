//! What each setting means, written once: docs/reference.md's settings
//! tables and the `rampire.toml(5)` man page are both made from these, and
//! a test holds the reference page to them and them to the settings the
//! configuration reads.

/// One setting, or settings described together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Setting {
    /// The names, as the reference's first column shows them: `` `pool` ``,
    /// or several, `` `install_path`, `service_account` ``.
    pub names: &'static str,
    /// The default, for a file whose settings have defaults.
    pub default: Option<&'static str>,
    pub meaning: &'static str,
}

/// One kind of settings file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingsFile {
    /// Its path in the configuration directory, as the reference's heading
    /// names it: `global.toml`.
    pub path: &'static str,
    /// What the reference says before the table.
    pub intro: &'static str,
    pub settings: &'static [Setting],
}

impl SettingsFile {
    /// Whether the table has a default column.
    pub fn has_defaults(&self) -> bool {
        self.settings
            .iter()
            .any(|setting| setting.default.is_some())
    }

    /// The table as docs/reference.md writes it.
    pub fn markdown_table(&self) -> String {
        let mut table = if self.has_defaults() {
            "| Setting | Default | Meaning |\n|---|---|---|\n".to_owned()
        } else {
            "| Setting | Meaning |\n|---|---|\n".to_owned()
        };
        for setting in self.settings {
            match setting.default {
                Some(default) if self.has_defaults() => table.push_str(&format!(
                    "| {} | {default} | {} |\n",
                    setting.names, setting.meaning
                )),
                _ if self.has_defaults() => table.push_str(&format!(
                    "| {} | none | {} |\n",
                    setting.names, setting.meaning
                )),
                _ => table.push_str(&format!("| {} | {} |\n", setting.names, setting.meaning)),
            }
        }
        table
    }

    /// Every setting's name, without the backticks.
    pub fn names(&self) -> Vec<&'static str> {
        self.settings
            .iter()
            .flat_map(|setting| setting.names.split(", "))
            .map(|name| name.trim_matches('`'))
            .collect()
    }
}

pub const GLOBAL: SettingsFile = SettingsFile {
    path: "global.toml",
    intro: "Every setting is optional.",
    settings: &[
        Setting {
            names: "`pool`",
            default: Some("none"),
            meaning: "Device memory shared by every VM, on top of each one's boot RAM. Required with more than one VM",
        },
        Setting {
            names: "`host_reserve`",
            default: Some("`\"4GB\"`"),
            meaning: "Memory the host keeps for itself; growth stops before crossing it",
        },
        Setting {
            names: "`observe_only`",
            default: Some("`false`"),
            meaning: "Decide and log everything, but change no VM's memory",
        },
        Setting {
            names: "`poll_interval_seconds`",
            default: Some("`5`"),
            meaning: "Seconds between host polls and guest samples, 1 to 10. A publication older than six polls is stale",
        },
        Setting {
            names: "`command_timeout_seconds`",
            default: Some("`10`"),
            meaning: "Limit on each libvirt command",
        },
        Setting {
            names: "`convergence_timeout_seconds`",
            default: Some("`300`"),
            meaning: "How long a Windows removal may stop moving before it is settled at what the guest returned, and the deadline for each settle command. Linux removals use a built-in 380 s ([administration](administration.md#how-the-settings-act))",
        },
        Setting {
            names: "`libvirt_uri`",
            default: Some("`\"qemu:///system\"`"),
            meaning: "The libvirt connection",
        },
        Setting {
            names: "`retention`",
            default: Some("`10`"),
            meaning: "Applied copies, and records of what each apply did, kept under `/var/lib/rampire`; at least 2, so there is always a copy to roll back to",
        },
    ],
};

pub const VM: SettingsFile = SettingsFile {
    path: "vms.d/VM.toml",
    intro: "One file per VM, named for the libvirt domain with `.toml` added:\nlower-case ASCII letters, digits, `-` and `_`, at most 64 characters. The\nname isn't repeated inside. Only `vms.d/*.toml` files are read, so editor\nbackups and `.rpmnew` files there are ignored.",
    settings: &[
        Setting {
            names: "`alias`",
            default: None,
            meaning: "The VM's virtio-mem device alias",
        },
        Setting {
            names: "`priority`",
            default: None,
            meaning: "Who is served first when memory is short; higher first",
        },
        Setting {
            names: "`min_ram`",
            default: None,
            meaning: "The least RAM the VM's system sees, boot RAM included; memory is never taken back below it. The guest's kernel keeps some boot RAM for itself, so a `min_ram` equal to boot RAM grows the VM a little at once",
        },
        Setting {
            names: "`max_ram`",
            default: None,
            meaning: "The largest it may grow. For Linux, at most about four times boot RAM",
        },
        Setting {
            names: "`free_ram`",
            default: None,
            meaning: "Spare memory the guest keeps; it asks for more below this and returns memory above it ([sizes](#sizes))",
        },
        Setting {
            names: "`guest_os`",
            default: None,
            meaning: "`\"linux\"`, or omitted for Windows",
        },
        Setting {
            names: "`review`",
            default: None,
            meaning: "Its review profile in `reviews/`",
        },
        Setting {
            names: "`telemetry_path`",
            default: None,
            meaning: "Optional host file for a diagnostic copy of each publication",
        },
        Setting {
            names: "`install_path`, `service_account`",
            default: None,
            meaning: "Windows only: where the guest service is installed and the account it runs as",
        },
    ],
};

pub const REVIEW: SettingsFile = SettingsFile {
    path: "reviews/PROFILE.toml",
    intro: "What RAMpire can't prove from the running VM. Each answer is\n`{ value = ..., source = \"detected\" | \"confirmed\" }`; `workload_reviewed`\ncan only be confirmed by a person. See the\n[example profile](../deploy/examples/reviews/linux-standard.toml).",
    settings: &[
        Setting {
            names: "`version`",
            default: None,
            meaning: "1",
        },
        Setting {
            names: "`guest_os`",
            default: None,
            meaning: "`\"linux\"` or `\"windows\"`, matching the VM",
        },
        Setting {
            names: "`workload_reviewed`",
            default: None,
            meaning: "The VM's programs cope with memory being added and taken away",
        },
        Setting {
            names: "`no_vdpa`, `no_rdma_migration`, `no_vfio_nvme`, `no_mlock`, `no_secure_virtualization`, `no_unsupported_vhost_user`",
            default: None,
            meaning: "None of these, each of which can hold on to guest memory, is used. `no_rdma_migration` is detected on a host with no RDMA hardware",
        },
        Setting {
            names: "`balloon_resize_inactive`",
            default: None,
            meaning: "The memory balloon isn't used to resize the VM; detected when there is none (`--fix-xml` turns it off)",
        },
        Setting {
            names: "`memory_slot_budget`, `vfio_mapping_budget`",
            default: None,
            meaning: "The budgets reviewed for the device",
        },
        Setting {
            names: "`guest_driver_version`",
            default: None,
            meaning: "The kernel release on Linux (`\"linux 6.12.0\"`), the `viomem` driver version on Windows",
        },
    ],
};

/// How sizes are written, as the reference's "Sizes" section says it.
pub const SIZES: &str =
    "Sizes are written in quotes with a unit: `\"512MB\"`, `\"3GB\"`, `\"1.5GB\"`.
Units are binary whatever the spelling: MB and MiB mean the same, and
1 GB = 1024 MB. Spaces and lower case are allowed (`\"3 gb\"`). A number
without quotes is a count of bytes, so `max_ram = 8` means 8 bytes and is
refused by the alignment rules below.

In a VM's file, `min_ram` and `max_ram` are multiples of 1 MiB, and
`free_ram` is a multiple of 8 MiB and less than `max_ram`. On the running
VM the limits are also aligned to its device's block, and `free_ram` must
be eight blocks or more: with the 2 MiB block `vm add` gives a new
device, use multiples of 16 MiB. Whole gigabytes always fit. Applying a
size that doesn't fit says which one.";

/// Every settings file, in the reference's order.
pub const FILES: [SettingsFile; 3] = [GLOBAL, VM, REVIEW];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{GlobalSettings, GuestOs, VmSettings};
    use std::collections::BTreeSet;

    fn root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn keys(text: &str) -> BTreeSet<String> {
        text.parse::<toml::Table>()
            .unwrap()
            .keys()
            .cloned()
            .collect()
    }

    fn named(file: &SettingsFile) -> BTreeSet<String> {
        file.names().into_iter().map(str::to_owned).collect()
    }

    #[test]
    fn the_reference_page_shows_these_tables_and_sizes() {
        let reference = std::fs::read_to_string(root().join("docs/reference.md")).unwrap();
        for file in FILES {
            let section = format!(
                "## `{}`\n\n{}\n\n{}",
                file.path,
                file.intro,
                file.markdown_table()
            );
            assert!(
                reference.contains(&section),
                "docs/reference.md's {} section differs from rampire-core's settings_reference; it should read:\n{section}",
                file.path
            );
        }
        assert!(
            reference.contains(&format!("## Sizes\n\n{SIZES}\n")),
            "docs/reference.md's Sizes section differs from settings_reference::SIZES"
        );
    }

    #[test]
    fn every_setting_the_configuration_reads_is_described() {
        let global = GlobalSettings {
            pool: Some(1 << 30),
            ..GlobalSettings::default()
        };
        assert_eq!(keys(&toml::to_string(&global).unwrap()), named(&GLOBAL));

        let vm = VmSettings {
            vm_name: String::new(),
            alias: "a".to_owned(),
            priority: 1,
            min_ram: 1,
            max_ram: 2,
            free_ram: 1,
            guest_os: GuestOs::Linux,
            install_path: Some(String::new()),
            service_account: Some(String::new()),
            telemetry_path: Some(String::new()),
            review: Some(String::new()),
        };
        assert_eq!(keys(&toml::to_string(&vm).unwrap()), named(&VM));

        let profile =
            std::fs::read_to_string(root().join("deploy/examples/reviews/linux-standard.toml"))
                .unwrap();
        assert_eq!(keys(&profile), named(&REVIEW));
    }
}
