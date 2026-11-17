//! A VM's recorded setup: the review it was checked against and its RAM
//! as it runs (its definition's memory and QEMU's virtio-mem options),
//! which every resize compares with the live VM before it goes ahead.
//! Nothing else about the VM is RAMpire's business.

use std::path::Path;

use rampire_core::{settings::GuestOs, CompatibilityEvidence};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

mod live_evidence;
mod memory_setup;

pub use live_evidence::{AttestedCompatibilitySource, VirshCompatibilityEvidenceSource};
pub use memory_setup::MemorySetup;

/// The attestation's version. Version 2 records the VM's RAM; version 1
/// fingerprinted its whole definition, QEMU's command line and the libvirt
/// and QEMU versions, and is recorded again by `apply`.
pub const COMPATIBILITY_ATTESTATION_VERSION: u32 = 2;

rampire_core::schema_name! {
    /// The `schema` of a VM's compatibility attestation.
    pub struct AttestationSchema = "rampire.attestation";
}
pub(crate) const MAX_ATTESTATION_BYTES: usize = 64 * 1024;

/// The review profile's version: `version` in `reviews/NAME.toml`.
pub const REVIEW_VERSION: u32 = 1;

/// How a VM that changed since its setup was recorded is described:
/// "libvirt was upgraded since RAMpire last checked this VM". `apply`
/// checks it again and records it.
pub const CHECKED: &str = "RAMpire last checked this VM";

/// Where a review answer came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnswerSource {
    /// Read from the domain's definition by tooling.
    Detected,
    /// Confirmed by a person.
    Confirmed,
}

/// One review answer and its source, written in TOML as
/// `no_vdpa = { value = true, source = "confirmed" }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Answer<T> {
    pub value: T,
    pub source: AnswerSource,
}

impl<T> Answer<T> {
    pub fn confirmed(value: T) -> Self {
        Self {
            value,
            source: AnswerSource::Confirmed,
        }
    }
}

/// A review profile, `reviews/NAME.toml`: the answers attestation needs that
/// the host can't prove from live evidence. One profile serves every VM of
/// a kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompatibilityReview {
    pub version: u32,
    /// The guest operating system reviewed. It must match the VM's settings,
    /// because the driver behaviour the host relies on differs between them.
    pub guest_os: GuestOs,
    /// The guest's workload tolerates memory being added and removed. Only a
    /// person can answer this.
    pub workload_reviewed: Answer<bool>,
    pub no_vdpa: Answer<bool>,
    pub no_rdma_migration: Answer<bool>,
    pub no_vfio_nvme: Answer<bool>,
    pub no_mlock: Answer<bool>,
    pub no_secure_virtualization: Answer<bool>,
    pub no_unsupported_vhost_user: Answer<bool>,
    pub balloon_resize_inactive: Answer<bool>,
    pub memory_slot_budget: Answer<u32>,
    pub vfio_mapping_budget: Answer<u32>,
    /// The guest's virtio-mem driver as reviewed: the `viomem` driver
    /// version on Windows (that path is PAUSED until the viomem driver bug is
    /// repaired), the kernel release on Linux.
    pub guest_driver_version: Answer<String>,
}

impl CompatibilityReview {
    fn validate(&self) -> Result<(), String> {
        if self.version != REVIEW_VERSION {
            return Err(format!(
                "unsupported review profile version {} (expected {REVIEW_VERSION})",
                self.version
            ));
        }
        if self.workload_reviewed.source != AnswerSource::Confirmed {
            return Err(
                "workload_reviewed can only be confirmed by a person, not detected".to_owned(),
            );
        }
        let confirmations = [
            (&self.workload_reviewed, "workload_reviewed"),
            (&self.no_vdpa, "no_vdpa"),
            (&self.no_rdma_migration, "no_rdma_migration"),
            (&self.no_vfio_nvme, "no_vfio_nvme"),
            (&self.no_mlock, "no_mlock"),
            (&self.no_secure_virtualization, "no_secure_virtualization"),
            (&self.no_unsupported_vhost_user, "no_unsupported_vhost_user"),
            (&self.balloon_resize_inactive, "balloon_resize_inactive"),
        ];
        if let Some((_, name)) = confirmations.iter().find(|(answer, _)| !answer.value) {
            return Err(format!("compatibility review does not confirm {name}"));
        }
        if self.memory_slot_budget.value == 0 {
            return Err("compatibility review memory_slot_budget must be positive".to_owned());
        }
        if self.vfio_mapping_budget.value == 0 {
            return Err("compatibility review vfio_mapping_budget must be positive".to_owned());
        }
        if self.guest_driver_version.value.trim().is_empty() {
            return Err("compatibility review guest_driver_version must be non-empty".to_owned());
        }
        Ok(())
    }
}

/// A VM's RAM as it runs: what its record holds and every resize
/// compares.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveCompatibilityEvidence {
    pub vm_name: String,
    pub device_alias: String,
    pub memory: MemorySetup,
    pub dynamic_memslots: bool,
    pub unplugged_inaccessible: bool,
}

impl LiveCompatibilityEvidence {
    fn validate(&self) -> Result<(), String> {
        if self.vm_name.trim().is_empty() {
            return Err("compatibility evidence VM name must be non-empty".to_owned());
        }
        validate_alias(&self.device_alias)?;
        if self.memory.device_size_bytes == 0 {
            return Err("compatibility evidence has an empty virtio-mem device".to_owned());
        }
        if !self.dynamic_memslots {
            return Err("compatibility evidence does not enable dynamic_memslots".to_owned());
        }
        if !self.unplugged_inaccessible {
            return Err("compatibility evidence does not enable unplugged_inaccessible".to_owned());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompatibilityAttestation {
    pub schema: AttestationSchema,
    pub version: u32,
    pub evidence: LiveCompatibilityEvidence,
    pub review: CompatibilityReview,
    pub fingerprint_sha256: String,
}

impl CompatibilityAttestation {
    pub fn new(
        evidence: LiveCompatibilityEvidence,
        review: CompatibilityReview,
    ) -> Result<Self, String> {
        evidence.validate()?;
        review.validate()?;
        let mut document = Self {
            schema: AttestationSchema,
            version: COMPATIBILITY_ATTESTATION_VERSION,
            evidence,
            review,
            fingerprint_sha256: String::new(),
        };
        document.fingerprint_sha256 = document.expected_fingerprint()?;
        Ok(document)
    }

    pub fn parse(json: &str) -> Result<Self, String> {
        // An earlier version's record doesn't parse as this one: say so.
        let version = serde_json::from_str::<Value>(json)
            .ok()
            .and_then(|value| value.get("version").and_then(Value::as_u64));
        if let Some(version) =
            version.filter(|&v| v != u64::from(COMPATIBILITY_ATTESTATION_VERSION))
        {
            return Err(format!(
                "its setup was recorded by another RAMpire (version {version}); rampire apply records it again"
            ));
        }
        let document: Self = serde_json::from_str(json)
            .map_err(|error| format!("parse compatibility attestation: {error}"))?;
        document.validate_integrity()?;
        Ok(document)
    }

    pub fn to_pretty_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self)
            .map_err(|error| format!("serialize compatibility attestation: {error}"))
    }

    pub fn validate_integrity(&self) -> Result<(), String> {
        if self.version != COMPATIBILITY_ATTESTATION_VERSION {
            return Err(format!(
                "unsupported compatibility attestation version {} (expected {COMPATIBILITY_ATTESTATION_VERSION}); attest the VM again",
                self.version
            ));
        }
        self.evidence.validate()?;
        self.review.validate()?;
        let expected = self.expected_fingerprint()?;
        if self.fingerprint_sha256 != expected {
            return Err("compatibility attestation fingerprint is invalid".to_owned());
        }
        Ok(())
    }

    pub fn validate_live(
        &self,
        live: &LiveCompatibilityEvidence,
        guest_os: GuestOs,
    ) -> Result<(), String> {
        self.validate_integrity()?;
        live.validate()?;
        if self.review.guest_os != guest_os {
            return Err(format!(
                "compatibility attestation reviewed a {:?} guest but the VM is configured as {guest_os:?}",
                self.review.guest_os
            ));
        }
        let recorded = &self.evidence;
        let what = if recorded.vm_name != live.vm_name {
            Some(format!(
                "its name went from {} to {}",
                recorded.vm_name, live.vm_name
            ))
        } else if recorded.device_alias != live.device_alias {
            Some(format!(
                "its virtio-mem device went from {} to {}",
                recorded.device_alias, live.device_alias
            ))
        } else if (recorded.dynamic_memslots, recorded.unplugged_inaccessible)
            != (live.dynamic_memslots, live.unplugged_inaccessible)
        {
            Some("QEMU's virtio-mem options changed".to_owned())
        } else {
            recorded.memory.changed(&live.memory)
        };
        match what {
            Some(what) => Err(format!("{what} since {CHECKED}")),
            None => Ok(()),
        }
    }

    fn expected_fingerprint(&self) -> Result<String, String> {
        let payload = serde_json::to_vec(&json!({
            "version": self.version,
            "evidence": self.evidence,
            "review": self.review,
        }))
        .map_err(|error| format!("serialize compatibility fingerprint payload: {error}"))?;
        Ok(sha256_hex(&payload))
    }
}

pub fn read_attestation(path: &Path) -> Result<CompatibilityAttestation, String> {
    let text = rampire_core::files::read_bounded_text(path, MAX_ATTESTATION_BYTES)
        .map_err(|error| format!("compatibility attestation: {error}"))?;
    CompatibilityAttestation::parse(&text)
}

/// A review profile, `reviews/NAME.toml`.
pub fn read_review(path: &Path) -> Result<CompatibilityReview, String> {
    let review: CompatibilityReview = rampire_core::settings::read_toml(path)?;
    review
        .validate()
        .map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(review)
}

fn evidence(value: bool) -> CompatibilityEvidence {
    if value {
        CompatibilityEvidence::Confirmed
    } else {
        CompatibilityEvidence::Rejected
    }
}

fn validate_alias(alias: &str) -> Result<(), String> {
    if rampire_core::settings::valid_alias(alias) {
        Ok(())
    } else {
        Err("compatibility evidence alias contains unsupported characters".to_owned())
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {

    use super::*;

    fn review() -> CompatibilityReview {
        let detected = |value| Answer {
            value,
            source: AnswerSource::Detected,
        };
        CompatibilityReview {
            version: REVIEW_VERSION,
            guest_os: GuestOs::Windows,
            workload_reviewed: Answer::confirmed(true),
            no_vdpa: detected(true),
            no_rdma_migration: Answer::confirmed(true),
            no_vfio_nvme: detected(true),
            no_mlock: detected(true),
            no_secure_virtualization: detected(true),
            no_unsupported_vhost_user: detected(true),
            balloon_resize_inactive: detected(true),
            memory_slot_budget: Answer::confirmed(32),
            vfio_mapping_budget: Answer::confirmed(8),
            guest_driver_version: Answer::confirmed("100.101.104.20800".to_owned()),
        }
    }

    #[test]
    fn the_documented_review_profile_loads() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../deploy/examples/reviews/linux-standard.toml");
        let review = read_review(&path).expect("example profile");
        assert_eq!(review.guest_os, GuestOs::Linux);
    }

    #[test]
    fn a_review_profile_is_toml_with_a_source_for_every_answer() {
        let directory = std::env::temp_dir().join(format!("rampire-review-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("linux-standard.toml");
        let text = r#"version = 1
guest_os = "linux"
workload_reviewed = { value = true, source = "confirmed" }
no_vdpa = { value = true, source = "detected" }
no_rdma_migration = { value = true, source = "confirmed" }
no_vfio_nvme = { value = true, source = "detected" }
no_mlock = { value = true, source = "detected" }
no_secure_virtualization = { value = true, source = "detected" }
no_unsupported_vhost_user = { value = true, source = "detected" }
balloon_resize_inactive = { value = true, source = "detected" }
memory_slot_budget = { value = 1, source = "confirmed" }
vfio_mapping_budget = { value = 1, source = "confirmed" }
guest_driver_version = { value = "linux 6.12", source = "confirmed" }
"#;
        std::fs::write(&path, text).unwrap();
        let read = read_review(&path);
        std::fs::write(
            &path,
            text.replace(
                "no_vdpa = { value = true, source = \"detected\" }",
                "no_vdpa = true",
            ),
        )
        .unwrap();
        let bare = read_review(&path);
        std::fs::write(
            &path,
            text.replace(
                "value = true, source = \"confirmed\" }\nno_vdpa",
                "value = true, source = \"detected\" }\nno_vdpa",
            ),
        )
        .unwrap();
        let detected_workload = read_review(&path);
        std::fs::remove_dir_all(&directory).unwrap();
        let review = read.expect("valid profile");
        assert_eq!(review.guest_os, GuestOs::Linux);
        assert_eq!(review.no_vdpa.source, AnswerSource::Detected);
        assert!(bare.is_err(), "every answer names its source");
        assert!(detected_workload.unwrap_err().contains("workload_reviewed"));
    }

    fn live() -> LiveCompatibilityEvidence {
        LiveCompatibilityEvidence {
            vm_name: "guest".to_owned(),
            device_alias: "memory0".to_owned(),
            memory: MemorySetup {
                max_memory_bytes: Some(9 << 30),
                max_memory_slots: Some(1),
                numa_cells_bytes: vec![1 << 30],
                device_size_bytes: 8 << 30,
                device_block_bytes: 2 << 20,
                device_node: Some(0),
                other_memory_devices: Vec::new(),
                balloon: Some("none".to_owned()),
                memory_backing: None,
            },
            dynamic_memslots: true,
            unplugged_inaccessible: true,
        }
    }

    #[test]
    fn a_setup_recorded_by_an_earlier_rampire_is_recorded_again() {
        // Version 1 fingerprinted the whole definition. It is refused, as
        // apply's re-recording needs, with what to do.
        let earlier = r#"{"schema":"rampire.attestation","version":1,"evidence":{"vm_name":"guest","device_alias":"memory0","domain_xml_sha256":"aa"},"review":{},"fingerprint_sha256":"bb"}"#;
        let error = CompatibilityAttestation::parse(earlier).unwrap_err();
        assert!(error.contains("rampire apply records it again"), "{error}");
    }

    #[test]
    fn round_trip_accepts_exact_reviewed_evidence() {
        let document = CompatibilityAttestation::new(live(), review()).expect("valid attestation");
        let parsed = CompatibilityAttestation::parse(&document.to_pretty_json().expect("JSON"))
            .expect("valid parsed attestation");
        assert_eq!(parsed.validate_live(&live(), GuestOs::Windows), Ok(()));
    }

    #[test]
    fn rejects_every_review_or_live_drift_category() {
        let document = CompatibilityAttestation::new(live(), review()).expect("valid attestation");
        let mut changes = Vec::new();
        let mut changed = live();
        changed.device_alias = "other".to_owned();
        changes.push(changed);
        let mut changed = live();
        changed.memory.max_memory_bytes = Some(16 << 30);
        changes.push(changed);
        let mut changed = live();
        changed.memory.numa_cells_bytes = vec![2 << 30];
        changes.push(changed);
        let mut changed = live();
        changed.memory.balloon = Some("virtio".to_owned());
        changes.push(changed);
        for changed in changes {
            let error = document
                .validate_live(&changed, GuestOs::Windows)
                .unwrap_err();
            assert!(error.ends_with(CHECKED), "{error}");
        }
        let mut changed = live();
        changed.memory.max_memory_bytes = Some(16 << 30);
        assert_eq!(
            document
                .validate_live(&changed, GuestOs::Windows)
                .unwrap_err(),
            format!("its maximum memory went from 9GB to 16GB since {CHECKED}")
        );
        // QEMU's options switched off are refused as they are read.
        let mut changed = live();
        changed.dynamic_memslots = false;
        assert!(document.validate_live(&changed, GuestOs::Windows).is_err());
        let mut changed = live();
        changed.unplugged_inaccessible = false;
        assert!(document.validate_live(&changed, GuestOs::Windows).is_err());

        assert!(document.validate_live(&live(), GuestOs::Linux).is_err());

        let mut tampered = document;
        tampered.review.vfio_mapping_budget.value += 1;
        assert!(tampered.validate_integrity().is_err());

        let mut unreviewed = review();
        unreviewed.no_vfio_nvme.value = false;
        assert!(CompatibilityAttestation::new(live(), unreviewed).is_err());

        let mut unsupported = CompatibilityAttestation::new(live(), review())
            .expect("valid attestation before version change");
        unsupported.version += 1;
        assert!(unsupported.validate_integrity().is_err());
    }
}
