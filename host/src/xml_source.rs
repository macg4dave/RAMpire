//! Reading a VM's virtio-mem device state from its live definition.

use rampire_core::{parse_virtio_mem_xml_for_alias, VirtioMemState};

use crate::virsh::VirshCommand;

/// Reads a VM's virtio-mem device state.
pub trait MemoryStateSource {
    fn memory_state(&self) -> Result<VirtioMemState, String>;

    /// Whether the VM is shut off, so its device holds no memory. Asked
    /// only when its state can't be read; anything else, a VM that has just
    /// started included, is not off.
    fn is_shut_off(&self) -> bool {
        false
    }
}

/// Whether a domain's live XML is a shut-off domain's: libvirt gives a
/// running one the `id` of its QEMU process and an inactive one none.
pub fn is_inactive_domain_xml(xml: &str) -> bool {
    xml.find("<domain")
        .and_then(|start| xml[start..].find('>').map(|end| &xml[start..start + end]))
        .is_some_and(|tag| !tag.contains(" id="))
}

pub struct VirshXmlSource<C> {
    command: C,
    vm_name: String,
    alias: String,
}

impl<C> VirshXmlSource<C> {
    pub fn new(command: C, vm_name: impl Into<String>, alias: impl Into<String>) -> Self {
        Self {
            command,
            vm_name: vm_name.into(),
            alias: alias.into(),
        }
    }
}

impl<C: VirshCommand> MemoryStateSource for VirshXmlSource<C> {
    fn memory_state(&self) -> Result<VirtioMemState, String> {
        let xml = self
            .command
            .run(&["dumpxml".to_owned(), self.vm_name.clone()])
            .map_err(|error| error.to_string())?;
        parse_virtio_mem_xml_for_alias(&xml, &self.alias)
            .map(|snapshot| snapshot.memory)
            .map_err(|error| error.to_string())
    }

    fn is_shut_off(&self) -> bool {
        self.command
            .run(&["dumpxml".to_owned(), self.vm_name.clone()])
            .is_ok_and(|xml| is_inactive_domain_xml(&xml))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::virsh::VirshError;
    struct Fake {
        xml: String,
    }
    impl VirshCommand for Fake {
        fn run(&self, arguments: &[String]) -> Result<String, VirshError> {
            assert_eq!(arguments, &["dumpxml".to_owned(), "guest".to_owned()]);
            Ok(self.xml.clone())
        }
    }
    #[test]
    fn a_domain_without_a_process_id_is_shut_off() {
        assert!(is_inactive_domain_xml(
            "<domain type='kvm'>\n  <name>guest</name>"
        ));
        assert!(!is_inactive_domain_xml(
            "<domain type='kvm' id='7'>\n  <name>guest</name>"
        ));
        // Unreadable XML is not taken for a VM that is off.
        assert!(!is_inactive_domain_xml(""));
    }

    #[test]
    fn reads_the_configured_alias_only() {
        let xml = "<domain><memory model='virtio-mem'><target><size unit='MiB'>8</size><block unit='MiB'>2</block><requested unit='MiB'>4</requested><current unit='MiB'>4</current></target><alias name='memory0'/></memory></domain>".to_owned();
        assert_eq!(
            VirshXmlSource::new(Fake { xml }, "guest", "memory0")
                .memory_state()
                .expect("valid state")
                .current_bytes,
            4 * 1024 * 1024
        );
    }
}
