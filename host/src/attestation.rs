use std::path::Path;

use rampire_core::{settings::GuestOs, CompatibilityEvidence};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

mod live_evidence;
mod runtime_scrub;

pub use live_evidence::{AttestedCompatibilitySource, VirshCompatibilityEvidenceSource};

/// Version 2 names the reviewed guest operating system and drops the
/// development-only trust flag of version 1; a version 1 attestation must be
/// made again from an updated review.
pub const COMPATIBILITY_ATTESTATION_VERSION: u32 = 1;

rampire_core::schema_name! {
    /// The `schema` of a VM's compatibility attestation.
    pub struct AttestationSchema = "rampire.attestation";
}
pub(crate) const MAX_ATTESTATION_BYTES: usize = 64 * 1024;

/// The review profile's version: `version` in `reviews/NAME.toml`.
pub const REVIEW_VERSION: u32 = 1;

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveCompatibilityEvidence {
    pub vm_name: String,
    pub device_alias: String,
    pub domain_xml_sha256: String,
    pub qemu_argv_sha256: String,
    pub libvirt_version_sha256: String,
    pub qemu_version: Value,
    pub dynamic_memslots: bool,
    pub unplugged_inaccessible: bool,
}

impl LiveCompatibilityEvidence {
    fn validate(&self) -> Result<(), String> {
        if self.vm_name.trim().is_empty() {
            return Err("compatibility evidence VM name must be non-empty".to_owned());
        }
        validate_alias(&self.device_alias)?;
        for (name, value) in [
            ("domain_xml_sha256", &self.domain_xml_sha256),
            ("qemu_argv_sha256", &self.qemu_argv_sha256),
            ("libvirt_version_sha256", &self.libvirt_version_sha256),
        ] {
            if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(format!("compatibility evidence {name} must be SHA-256 hex"));
            }
        }
        let qemu = self
            .qemu_version
            .get("qemu")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                "compatibility evidence qemu_version.qemu must be an object".to_owned()
            })?;
        for component in ["major", "minor", "micro"] {
            if qemu.get(component).and_then(Value::as_u64).is_none() {
                return Err(format!(
                    "compatibility evidence qemu_version.qemu.{component} must be an unsigned integer"
                ));
            }
        }
        if self
            .qemu_version
            .get("package")
            .and_then(Value::as_str)
            .is_none()
        {
            return Err("compatibility evidence qemu_version.package must be a string".to_owned());
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
        // Each field with its attested and live values, so a drift report
        // identifies what changed.
        let comparisons = [
            (
                "vm_name",
                self.evidence.vm_name.clone(),
                live.vm_name.clone(),
            ),
            (
                "device_alias",
                self.evidence.device_alias.clone(),
                live.device_alias.clone(),
            ),
            (
                "domain_xml",
                self.evidence.domain_xml_sha256.clone(),
                live.domain_xml_sha256.clone(),
            ),
            (
                "qemu_argv",
                self.evidence.qemu_argv_sha256.clone(),
                live.qemu_argv_sha256.clone(),
            ),
            (
                "libvirt_version",
                self.evidence.libvirt_version_sha256.clone(),
                live.libvirt_version_sha256.clone(),
            ),
            (
                "qemu_version",
                self.evidence.qemu_version.to_string(),
                live.qemu_version.to_string(),
            ),
            (
                "dynamic_memslots",
                self.evidence.dynamic_memslots.to_string(),
                live.dynamic_memslots.to_string(),
            ),
            (
                "unplugged_inaccessible",
                self.evidence.unplugged_inaccessible.to_string(),
                live.unplugged_inaccessible.to_string(),
            ),
        ];
        if let Some((field, attested, observed)) = comparisons
            .iter()
            .find(|(_, attested, observed)| attested != observed)
        {
            return Err(format!(
                "live compatibility evidence drifted: {field} (attested {attested}, live {observed})"
            ));
        }
        Ok(())
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
            domain_xml_sha256: "a".repeat(64),
            qemu_argv_sha256: "b".repeat(64),
            libvirt_version_sha256: "c".repeat(64),
            qemu_version: json!({"qemu":{"major":9,"minor":2,"micro":0},"package":"qemu-9.2"}),
            dynamic_memslots: true,
            unplugged_inaccessible: true,
        }
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
        changed.domain_xml_sha256 = "d".repeat(64);
        changes.push(changed);
        let mut changed = live();
        changed.qemu_argv_sha256 = "e".repeat(64);
        changes.push(changed);
        let mut changed = live();
        changed.libvirt_version_sha256 = "f".repeat(64);
        changes.push(changed);
        let mut changed = live();
        changed.qemu_version = json!({"qemu":{"major":10,"minor":0,"micro":0},"package":"qemu-10"});
        changes.push(changed);
        let mut changed = live();
        changed.dynamic_memslots = false;
        changes.push(changed);
        let mut changed = live();
        changed.unplugged_inaccessible = false;
        changes.push(changed);
        for changed in changes {
            assert!(document.validate_live(&changed, GuestOs::Windows).is_err());
        }

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
