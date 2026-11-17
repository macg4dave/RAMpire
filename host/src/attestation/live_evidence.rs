//! The live side of attestation: a running VM's RAM (its definition's
//! memory, read through virsh, and its virtio-mem device's options, read
//! through QMP), and the compatibility source that holds it against the
//! VM's attestation file.

use std::cell::RefCell;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use rampire_core::{
    parse_virtio_mem_xml_for_alias, settings::GuestOs, CompatibilityEvidence,
    VirtioMemCompatibility, VirtioMemXmlError,
};
use serde_json::{json, Value};

use super::{evidence, read_attestation, validate_alias, LiveCompatibilityEvidence, MemorySetup};
use crate::compatibility_source::{CompatibilityError, CompatibilitySource};
use crate::virsh::VirshCommand;

/// How long the device's options, fixed while one QEMU process runs, are
/// reused before they are read again. The domain XML is read and checked
/// on every collection.
const PROCESS_EVIDENCE_MAX_AGE: Duration = Duration::from_secs(300);

/// The virtio-mem device's options, fixed while one QEMU process runs.
#[derive(Debug, Clone)]
struct ProcessEvidence {
    dynamic_memslots: bool,
    unplugged_inaccessible: bool,
}

pub struct VirshCompatibilityEvidenceSource<C> {
    command: C,
    vm_name: String,
    alias: String,
    /// The process evidence last collected, with the runtime domain id it
    /// belongs to and when. Collecting it takes two QMP commands; a resize
    /// is checked against it many times a minute.
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
    pub fn collect(&self) -> Result<LiveCompatibilityEvidence, CompatibilityError> {
        validate_alias(&self.alias)?;
        let domain_xml = self.run("live domain XML", &["dumpxml", &self.vm_name])?;
        // The device must be there and whole. Its `current` is allocation
        // state, left out of the fingerprint, and libvirt shows it only once
        // QEMU has reported the device's size: just after the host starts,
        // a running VM may not have it yet. Refusing it then failed every
        // VM's check, so no apply, add or remove could go through.
        match parse_virtio_mem_xml_for_alias(&domain_xml, &self.alias) {
            Ok(_) | Err(VirtioMemXmlError::MissingField("current")) => {}
            Err(error) => {
                return Err(CompatibilityError::Refused(format!(
                    "validate live domain XML for attestation: {error}"
                )))
            }
        }
        let memory = MemorySetup::read(&domain_xml, &self.alias).map_err(|error| {
            CompatibilityError::Refused(format!("read the live domain's RAM: {error}"))
        })?;
        let process = self.process_evidence(runtime_domain_id(&domain_xml))?;
        Ok(LiveCompatibilityEvidence {
            vm_name: self.vm_name.clone(),
            device_alias: self.alias.clone(),
            memory,
            dynamic_memslots: process.dynamic_memslots,
            unplugged_inaccessible: process.unplugged_inaccessible,
        })
    }

    /// The process evidence for the domain running as `domain_id`: reused
    /// while that id is unchanged and younger than
    /// `PROCESS_EVIDENCE_MAX_AGE`, otherwise collected again. Without an id
    /// it is always collected.
    fn process_evidence(
        &self,
        domain_id: Option<&str>,
    ) -> Result<ProcessEvidence, CompatibilityError> {
        if let (Some(id), Some((cached_id, collected, evidence))) =
            (domain_id, self.process.borrow().as_ref())
        {
            if cached_id == id && collected.elapsed() < PROCESS_EVIDENCE_MAX_AGE {
                return Ok(evidence.clone());
            }
        }
        let evidence = ProcessEvidence {
            dynamic_memslots: self.qom_bool("dynamic-memslots")?,
            unplugged_inaccessible: self.qom_bool("unplugged-inaccessible")?,
        };
        *self.process.borrow_mut() =
            domain_id.map(|id| (id.to_owned(), Instant::now(), evidence.clone()));
        Ok(evidence)
    }

    fn run(&self, description: &str, arguments: &[&str]) -> Result<String, CompatibilityError> {
        let output = self
            .command
            .run(
                &arguments
                    .iter()
                    .map(|value| (*value).to_owned())
                    .collect::<Vec<_>>(),
            )
            .map_err(|error| {
                let reason = format!("read {description}: {error}");
                if error.is_unavailable() {
                    CompatibilityError::Unavailable(reason)
                } else {
                    CompatibilityError::Refused(reason)
                }
            })?;
        if output.trim().is_empty() {
            Err(CompatibilityError::Refused(format!(
                "read {description}: command returned empty output"
            )))
        } else {
            Ok(output)
        }
    }

    fn qmp(&self, execute: &str, arguments: Option<Value>) -> Result<Value, CompatibilityError> {
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
        Ok(response
            .get("return")
            .cloned()
            .ok_or_else(|| "live QMP evidence response is missing return".to_owned())?)
    }

    fn qom_bool(&self, property: &str) -> Result<bool, CompatibilityError> {
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
            _ => Err(CompatibilityError::Refused(format!(
                "live QMP property {property} has unsupported value {value}"
            ))),
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
    fn compatibility(&self) -> Result<VirtioMemCompatibility, CompatibilityError> {
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

    const QMP_ON: [&str; 2] = [r#"{"return":true}"#, r#"{"return":"on"}"#];

    fn domain(id: u32, requested: &str, current: &str) -> String {
        format!("<domain type='kvm' id='{id}'><maxMemory slots='1' unit='KiB'>9437184</maxMemory><cpu><numa><cell id='0' cpus='0' memory='1048576' unit='KiB'/></numa></cpu><devices><memory model='virtio-mem'><target><size unit='GiB'>8</size><node>0</node><block unit='MiB'>2</block>{requested}{current}</target><alias name='memory0'/></memory></devices></domain>")
    }

    fn responses(xml: String) -> Vec<String> {
        let mut all = vec![xml];
        all.extend(QMP_ON.map(str::to_owned));
        all
    }

    #[test]
    fn collector_reads_the_ram_and_parses_qmp_booleans() {
        let requested = "<requested unit='GiB'>1</requested>";
        let source = VirshCompatibilityEvidenceSource::new(
            FakeVirsh::new(responses(domain(7, requested, ""))),
            "guest",
            "memory0",
        );
        let evidence = source.collect().expect("complete evidence");
        assert!(evidence.dynamic_memslots && evidence.unplugged_inaccessible);
        assert_eq!(evidence.memory.device_size_bytes, 8 << 30);
        assert_eq!(evidence.memory.numa_cells_bytes, vec![1 << 30]);
        assert_eq!(evidence.memory.max_memory_bytes, Some(9 << 30));
        assert!(
            source.collect().is_err(),
            "fixture exhausted: only the XML and two QMP reads were asked"
        );
    }

    #[test]
    fn a_device_whose_current_size_libvirt_hasnt_shown_yet_is_whole() {
        // Every managed VM on 2026-11-16, just after the host restarted:
        // libvirt had no <current> for their devices yet, and every apply
        // on the host failed until it did.
        let evidence = |current: &str, requested: &str| {
            VirshCompatibilityEvidenceSource::new(
                FakeVirsh::new(responses(domain(7, requested, current))),
                "guest",
                "memory0",
            )
            .collect()
        };
        let requested = "<requested unit='KiB'>0</requested>";
        let without = evidence("", requested).expect("no current yet is whole");
        let with = evidence("<current unit='KiB'>0</current>", requested).unwrap();
        // The same: the device's size now isn't part of its setup.
        assert_eq!(without, with);
        // Anything else missing is still a broken device.
        let error = evidence("<current unit='KiB'>0</current>", "").unwrap_err();
        assert!(error.to_string().contains("missing requested"), "{error}");
    }

    #[test]
    fn process_evidence_is_reused_until_the_domain_restarts() {
        let requested = |gib: u32| format!("<requested unit='GiB'>{gib}</requested>");
        // Same QEMU process: only the XML is read again. A restart (new id)
        // reads the options again. The fixture runs out if more is asked.
        let mut all = responses(domain(7, &requested(1), ""));
        all.push(domain(7, &requested(2), ""));
        all.extend(responses(domain(8, &requested(1), "")));
        let source = VirshCompatibilityEvidenceSource::new(FakeVirsh::new(all), "guest", "memory0");
        let first = source.collect().expect("first");
        let same = source.collect().expect("same process");
        let restarted = source.collect().expect("restarted");
        assert_eq!(first, same);
        assert_eq!(first, restarted);
        assert!(
            source.collect().is_err(),
            "fixture exhausted: nothing extra was run"
        );

        assert_eq!(runtime_domain_id("<domain type='kvm' id='12'>"), Some("12"));
        assert_eq!(runtime_domain_id("<domain type='kvm'>"), None);
    }
}
