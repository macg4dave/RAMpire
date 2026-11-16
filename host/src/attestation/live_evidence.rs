//! The live side of attestation: the evidence a running VM shows (its
//! domain definition, QEMU process and device properties, read through
//! virsh and QMP), and the compatibility source that holds it against the
//! VM's attestation file.

use std::cell::RefCell;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use rampire_core::{
    parse_virtio_mem_xml_for_alias, settings::GuestOs, CompatibilityEvidence,
    VirtioMemCompatibility, VirtioMemXmlError,
};
use serde_json::{json, Value};

use super::runtime_scrub::{
    scrub_allocation_state, scrub_descriptor_numbers, scrub_qemu_argv, scrub_runtime_identity,
};
use super::{evidence, read_attestation, sha256_hex, validate_alias, LiveCompatibilityEvidence};
use crate::compatibility_source::CompatibilitySource;
use crate::virsh::VirshCommand;

/// How long evidence fixed for one QEMU process is reused before it is
/// collected again. The QEMU arguments, QEMU version and device properties
/// cannot change while that process runs; the libvirt version changes only
/// with an upgrade. The domain XML is read and checked on every collection.
const PROCESS_EVIDENCE_MAX_AGE: Duration = Duration::from_secs(300);

/// Evidence that is fixed while one QEMU process runs.
#[derive(Debug, Clone)]
struct ProcessEvidence {
    qemu_argv_sha256: String,
    libvirt_version_sha256: String,
    qemu_version: Value,
    dynamic_memslots: bool,
    unplugged_inaccessible: bool,
}

pub struct VirshCompatibilityEvidenceSource<C> {
    command: C,
    vm_name: String,
    alias: String,
    /// The process evidence last collected, with the runtime domain id it
    /// belongs to and when. Collecting it takes five `virsh` commands; a
    /// resize is checked against it many times a minute.
    process: RefCell<Option<(String, Instant, ProcessEvidence)>>,
}

impl<C> VirshCompatibilityEvidenceSource<C> {
    pub fn new(command: C, vm_name: impl Into<String>, alias: impl Into<String>) -> Self {
        Self {
            command,
            vm_name: vm_name.into(),
            alias: alias.into(),
            process: RefCell::new(None),
        }
    }
}

/// The runtime `id` of a running domain (`<domain type='kvm' id='7'>`), which
/// libvirt assigns afresh each time the domain starts.
fn runtime_domain_id(domain_xml: &str) -> Option<&str> {
    let start = domain_xml.find("<domain ")?;
    let tag = &domain_xml[start..start + domain_xml[start..].find('>')?];
    let value = &tag[tag.find(" id='")? + " id='".len()..];
    Some(&value[..value.find('\'')?])
}

impl<C: VirshCommand> VirshCompatibilityEvidenceSource<C> {
    pub fn collect(&self) -> Result<LiveCompatibilityEvidence, String> {
        validate_alias(&self.alias)?;
        let domain_xml = self.run("live domain XML", &["dumpxml", &self.vm_name])?;
        // The device must be there and whole. Its `current` is allocation
        // state, left out of the fingerprint, and libvirt shows it only once
        // QEMU has reported the device's size: just after the host starts,
        // a running VM may not have it yet. Refusing it then failed every
        // VM's check, so no apply, add or remove could go through.
        match parse_virtio_mem_xml_for_alias(&domain_xml, &self.alias) {
            Ok(_) | Err(VirtioMemXmlError::MissingField("current")) => {}
            Err(error) => return Err(format!("validate live domain XML for attestation: {error}")),
        }
        let process = self.process_evidence(runtime_domain_id(&domain_xml))?;
        Ok(LiveCompatibilityEvidence {
            vm_name: self.vm_name.clone(),
            device_alias: self.alias.clone(),
            domain_xml_sha256: sha256_hex(
                scrub_runtime_identity(&scrub_allocation_state(&domain_xml)).as_bytes(),
            ),
            qemu_argv_sha256: process.qemu_argv_sha256,
            libvirt_version_sha256: process.libvirt_version_sha256,
            qemu_version: process.qemu_version,
            dynamic_memslots: process.dynamic_memslots,
            unplugged_inaccessible: process.unplugged_inaccessible,
        })
    }

    /// The process evidence for the domain running as `domain_id`: reused
    /// while that id is unchanged and younger than
    /// `PROCESS_EVIDENCE_MAX_AGE`, otherwise collected again. Without an id
    /// it is always collected.
    fn process_evidence(&self, domain_id: Option<&str>) -> Result<ProcessEvidence, String> {
        if let (Some(id), Some((cached_id, collected, evidence))) =
            (domain_id, self.process.borrow().as_ref())
        {
            if cached_id == id && collected.elapsed() < PROCESS_EVIDENCE_MAX_AGE {
                return Ok(evidence.clone());
            }
        }
        let qemu_argv = self.run(
            "live QEMU argv",
            &["domxml-to-native", "qemu-argv", "--domain", &self.vm_name],
        )?;
        let libvirt_version = self.run("libvirt version", &["version"])?;
        let evidence = ProcessEvidence {
            qemu_argv_sha256: sha256_hex(
                scrub_descriptor_numbers(&scrub_qemu_argv(&qemu_argv))
                    .trim()
                    .as_bytes(),
            ),
            libvirt_version_sha256: sha256_hex(libvirt_version.trim().as_bytes()),
            qemu_version: self.qmp("query-version", None)?,
            dynamic_memslots: self.qom_bool("dynamic-memslots")?,
            unplugged_inaccessible: self.qom_bool("unplugged-inaccessible")?,
        };
        *self.process.borrow_mut() =
            domain_id.map(|id| (id.to_owned(), Instant::now(), evidence.clone()));
        Ok(evidence)
    }

    fn run(&self, description: &str, arguments: &[&str]) -> Result<String, String> {
        let output = self
            .command
            .run(
                &arguments
                    .iter()
                    .map(|value| (*value).to_owned())
                    .collect::<Vec<_>>(),
            )
            .map_err(|error| format!("read {description}: {error}"))?;
        if output.trim().is_empty() {
            Err(format!("read {description}: command returned empty output"))
        } else {
            Ok(output)
        }
    }

    fn qmp(&self, execute: &str, arguments: Option<Value>) -> Result<Value, String> {
        let mut request = json!({"execute": execute});
        if let Some(arguments) = arguments {
            request["arguments"] = arguments;
        }
        let response = self.run(
            "live QMP evidence",
            &["qemu-monitor-command", &self.vm_name, &request.to_string()],
        )?;
        let response: Value = serde_json::from_str(&response)
            .map_err(|error| format!("parse live QMP evidence: {error}"))?;
        response
            .get("return")
            .cloned()
            .ok_or_else(|| "live QMP evidence response is missing return".to_owned())
    }

    fn qom_bool(&self, property: &str) -> Result<bool, String> {
        let path = format!("/machine/peripheral/{}", self.alias);
        let value = self.qmp("qom-get", Some(json!({"path": path, "property": property})))?;
        match value {
            Value::Bool(value) => Ok(value),
            Value::String(value) if matches!(value.as_str(), "on" | "yes" | "true" | "1") => {
                Ok(true)
            }
            Value::String(value) if matches!(value.as_str(), "off" | "no" | "false" | "0") => {
                Ok(false)
            }
            _ => Err(format!(
                "live QMP property {property} has unsupported value {value}"
            )),
        }
    }
}

pub struct AttestedCompatibilitySource<C> {
    live: VirshCompatibilityEvidenceSource<C>,
    path: PathBuf,
    guest_os: GuestOs,
}

impl<C> AttestedCompatibilitySource<C> {
    pub fn new(
        command: C,
        vm_name: impl Into<String>,
        alias: impl Into<String>,
        guest_os: GuestOs,
        path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            live: VirshCompatibilityEvidenceSource::new(command, vm_name, alias),
            path: path.into(),
            guest_os,
        }
    }
}

impl<C: VirshCommand> CompatibilitySource for AttestedCompatibilitySource<C> {
    fn compatibility(&self) -> Result<VirtioMemCompatibility, String> {
        let attestation = read_attestation(&self.path)?;
        let live = self.live.collect()?;
        attestation.validate_live(&live, self.guest_os)?;
        Ok(VirtioMemCompatibility {
            dynamic_memslots: evidence(live.dynamic_memslots),
            unplugged_inaccessible: evidence(live.unplugged_inaccessible),
            workload_review: CompatibilityEvidence::Confirmed,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;
    use crate::virsh::VirshError;

    struct FakeVirsh {
        responses: RefCell<VecDeque<Result<String, VirshError>>>,
    }
    impl FakeVirsh {
        fn new(values: impl IntoIterator<Item = String>) -> Self {
            Self {
                responses: RefCell::new(values.into_iter().map(Ok).collect()),
            }
        }
    }
    impl VirshCommand for FakeVirsh {
        fn run(&self, _arguments: &[String]) -> Result<String, VirshError> {
            self.responses
                .borrow_mut()
                .pop_front()
                .ok_or_else(|| VirshError::Failed {
                    status: "fixture exhausted".to_owned(),
                    stderr: String::new(),
                })?
        }
    }

    #[test]
    fn collector_hashes_full_configuration_and_parses_qmp_booleans() {
        let source = VirshCompatibilityEvidenceSource::new(FakeVirsh::new([
            "<domain><memory model='virtio-mem' dynamic-memslots='on' unplugged-inaccessible='on'><target><size unit='GiB'>8</size><block unit='MiB'>2</block><requested unit='GiB'>1</requested><current unit='GiB'>1</current></target><alias name='memory0'/></memory></domain>".to_owned(),
            "qemu-system -machine q35 -object memory-backend-file,id=mem0".to_owned(),
            "Compiled against library: libvirt 11.0.0".to_owned(),
            r#"{"return":{"qemu":{"major":9,"minor":2,"micro":0},"package":"qemu-9.2"}}"#.to_owned(),
            r#"{"return":true}"#.to_owned(), r#"{"return":"on"}"#.to_owned(),
        ]), "guest", "memory0");
        let evidence = source.collect().expect("complete evidence");
        assert!(evidence.dynamic_memslots && evidence.unplugged_inaccessible);
        assert_eq!(evidence.domain_xml_sha256.len(), 64);
    }

    #[test]
    fn a_device_whose_current_size_libvirt_hasnt_shown_yet_is_whole() {
        // Every managed VM on 2026-11-16, just after the host restarted:
        // libvirt had no <current> for their devices yet, and every apply
        // on the host failed until it did.
        let evidence = |current: &str, requested: &str| {
            VirshCompatibilityEvidenceSource::new(FakeVirsh::new([
                format!("<domain><memory model='virtio-mem' dynamic-memslots='on' unplugged-inaccessible='on'><target><size unit='GiB'>8</size><block unit='MiB'>2</block>{requested}{current}</target><alias name='memory0'/></memory></domain>"),
                "qemu-system -machine q35".to_owned(),
                "Compiled against library: libvirt 11.0.0".to_owned(),
                r#"{"return":{"qemu":{"major":9,"minor":2,"micro":0},"package":"qemu-9.2"}}"#.to_owned(),
                r#"{"return":true}"#.to_owned(), r#"{"return":true}"#.to_owned(),
            ]), "guest", "memory0").collect()
        };
        let requested = "<requested unit='KiB'>0</requested>";
        let without = evidence("", requested).expect("no current yet is whole");
        let with = evidence("<current unit='KiB'>0</current>", requested).unwrap();
        // The same fingerprint: current is left out of it either way.
        assert_eq!(without.domain_xml_sha256, with.domain_xml_sha256);
        // Anything else missing is still a broken device.
        let error = evidence("<current unit='KiB'>0</current>", "").unwrap_err();
        assert!(error.contains("missing requested"), "{error}");
    }

    #[test]
    fn process_evidence_is_reused_until_the_domain_restarts() {
        let xml = |id: u32, requested: u32| {
            format!("<domain type='kvm' id='{id}'><memory model='virtio-mem' dynamic-memslots='on' unplugged-inaccessible='on'><target><size unit='GiB'>8</size><block unit='MiB'>2</block><requested unit='GiB'>{requested}</requested><current unit='GiB'>{requested}</current></target><alias name='memory0'/></memory></domain>")
        };
        let process = |argv: &str| {
            [
                argv.to_owned(),
                "Compiled against library: libvirt 11.0.0".to_owned(),
                r#"{"return":{"qemu":{"major":9,"minor":2,"micro":0},"package":"qemu-9.2"}}"#
                    .to_owned(),
                r#"{"return":true}"#.to_owned(),
                r#"{"return":true}"#.to_owned(),
            ]
        };
        // Same QEMU process: only the XML is read again. A restart (new id)
        // collects everything again. The fixture runs out if more is asked.
        let mut responses = vec![xml(7, 1)];
        responses.extend(process("qemu-system -machine q35"));
        responses.push(xml(7, 2));
        responses.push(xml(8, 1));
        responses.extend(process("qemu-system -machine q35 -m 2G"));
        let source =
            VirshCompatibilityEvidenceSource::new(FakeVirsh::new(responses), "guest", "memory0");
        let first = source.collect().expect("first");
        let same = source.collect().expect("same process");
        assert_eq!(first.qemu_argv_sha256, same.qemu_argv_sha256);
        let restarted = source.collect().expect("restarted");
        assert_ne!(first.qemu_argv_sha256, restarted.qemu_argv_sha256);
        assert!(
            source.collect().is_err(),
            "fixture exhausted: nothing extra was run"
        );

        assert_eq!(runtime_domain_id("<domain type='kvm' id='12'>"), Some("12"));
        assert_eq!(runtime_domain_id("<domain type='kvm'>"), None);
    }
}
