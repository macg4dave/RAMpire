use quick_xml::events::Event;
use quick_xml::{Reader, XmlVersion};
use thiserror::Error;

use crate::units::libvirt_unit_bytes;
use crate::virtio_mem::VirtioMemState;
use crate::{CompatibilityEvidence, VirtioMemCompatibility, VirtioMemError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtioMemXmlState {
    pub alias: String,
    pub memory: VirtioMemState,
    pub compatibility: VirtioMemCompatibility,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum VirtioMemXmlError {
    #[error("invalid virtio-mem XML: {0}")]
    InvalidXml(String),
    #[error("virtio-mem XML does not contain a memory model")]
    MissingMemoryModel,
    #[error("virtio-mem XML has an unexpected model: {0}")]
    UnexpectedModel(String),
    #[error("virtio-mem XML is missing {0}")]
    MissingField(&'static str),
    #[error("virtio-mem XML field {field} has invalid value {value}")]
    InvalidValue { field: &'static str, value: String },
    #[error("virtio-mem XML field {field} uses unsupported unit {unit}")]
    UnsupportedUnit { field: &'static str, unit: String },
    #[error("virtio-mem XML field {field} overflows when converted to bytes")]
    Overflow { field: &'static str },
    #[error("virtio-mem XML does not contain alias {0}")]
    AliasNotFound(String),
    #[error("virtio-mem XML contains alias {0} more than once")]
    DuplicateAlias(String),
    #[error(transparent)]
    InvalidState(#[from] VirtioMemError),
}

pub fn parse_virtio_mem_xml(xml: &str) -> Result<VirtioMemXmlState, VirtioMemXmlError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut memory_depth = None;
    // The field awaiting its value, its index and its unit in bytes.
    let mut pending: Option<(&'static str, usize, u64)> = None;
    let mut alias = None;
    let mut compatibility = VirtioMemCompatibility::unknown();
    let mut values: [Option<u64>; 4] = [None; 4];

    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(element)) => {
                let name = element.name().as_ref().to_vec();
                if name == b"memory" {
                    let model = attribute(&element, b"model")?;
                    compatibility.dynamic_memslots =
                        compatibility_attribute(&element, b"dynamic-memslots", b"dynamicMemslots")?;
                    compatibility.unplugged_inaccessible = compatibility_attribute(
                        &element,
                        b"unplugged-inaccessible",
                        b"unpluggedInaccessible",
                    )?;
                    memory_depth = Some(1_u32);
                    match model.as_deref() {
                        Some("virtio-mem") => {}
                        Some(model) => {
                            return Err(VirtioMemXmlError::UnexpectedModel(model.to_owned()))
                        }
                        None => return Err(VirtioMemXmlError::MissingMemoryModel),
                    }
                } else if memory_depth.is_some() {
                    if name == b"alias" {
                        alias = attribute(&element, b"name")?;
                    } else if let Some((field, index)) = field_name(&name) {
                        let unit = attribute(&element, b"unit")?;
                        let multiplier = libvirt_unit_bytes(unit.as_deref()).ok_or_else(|| {
                            VirtioMemXmlError::UnsupportedUnit {
                                field,
                                unit: unit.unwrap_or_default(),
                            }
                        })?;
                        pending = Some((field, index, multiplier));
                    }
                    if let Some(depth) = memory_depth.as_mut() {
                        *depth += 1;
                    }
                }
            }
            Ok(Event::Empty(element)) if memory_depth.is_some() => {
                if element.name().as_ref() == b"alias" {
                    alias = attribute(&element, b"name")?;
                }
            }
            Ok(Event::Text(text)) => {
                if let Some((field, index, multiplier)) = pending.take() {
                    let raw = text
                        .decode()
                        .map_err(|error| VirtioMemXmlError::InvalidXml(error.to_string()))?;
                    let value =
                        raw.parse::<u64>()
                            .map_err(|_| VirtioMemXmlError::InvalidValue {
                                field,
                                value: raw.into_owned(),
                            })?;
                    values[index] = Some(
                        value
                            .checked_mul(multiplier)
                            .ok_or(VirtioMemXmlError::Overflow { field })?,
                    );
                }
            }
            Ok(Event::End(element)) => {
                if element.name().as_ref() == b"memory" {
                    memory_depth = None;
                } else if let Some(depth) = memory_depth.as_mut() {
                    *depth = depth.saturating_sub(1);
                }
            }
            Ok(Event::Eof) => break,
            Err(error) => return Err(VirtioMemXmlError::InvalidXml(error.to_string())),
            _ => {}
        }
        buffer.clear();
    }

    let alias = alias.ok_or(VirtioMemXmlError::MissingField("alias"))?;
    let memory = VirtioMemState {
        size_bytes: bytes(values[0], "size")?,
        block_size_bytes: bytes(values[1], "block")?,
        requested_bytes: bytes(values[2], "requested")?,
        current_bytes: bytes(values[3], "current")?,
    };
    memory.validate()?;
    Ok(VirtioMemXmlState {
        alias,
        memory,
        compatibility,
    })
}

/// Parse exactly one virtio-mem device selected by its libvirt alias.
///
/// Non-virtio-mem memory devices are ignored. The selected alias must occur
/// exactly once; callers must not infer a device from document order.
pub fn parse_virtio_mem_xml_for_alias(
    xml: &str,
    expected_alias: &str,
) -> Result<VirtioMemXmlState, VirtioMemXmlError> {
    let mut reader = Reader::from_str(xml);
    let mut buffer = Vec::new();
    let mut candidate_start = None;
    let mut candidate_depth = 0_u32;
    let mut selected = None;

    loop {
        let event_start = usize::try_from(reader.buffer_position())
            .map_err(|_| VirtioMemXmlError::InvalidXml("XML document is too large".to_owned()))?;
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(element)) => {
                if candidate_start.is_some() {
                    candidate_depth += 1;
                } else if element.name().as_ref() == b"memory"
                    && attribute(&element, b"model")?.as_deref() == Some("virtio-mem")
                {
                    candidate_start = Some(event_start);
                    candidate_depth = 1;
                }
            }
            Ok(Event::End(_)) if candidate_start.is_some() => {
                candidate_depth = candidate_depth.saturating_sub(1);
                if candidate_depth == 0 {
                    let start = candidate_start.take().ok_or_else(|| {
                        VirtioMemXmlError::InvalidXml(
                            "parser lost memory-device boundary".to_owned(),
                        )
                    })?;
                    let end = usize::try_from(reader.buffer_position()).map_err(|_| {
                        VirtioMemXmlError::InvalidXml("XML document is too large".to_owned())
                    })?;
                    let snapshot = parse_virtio_mem_xml(&xml[start..end])?;
                    if snapshot.alias == expected_alias {
                        if selected.is_some() {
                            return Err(VirtioMemXmlError::DuplicateAlias(
                                expected_alias.to_owned(),
                            ));
                        }
                        selected = Some(snapshot);
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(error) => return Err(VirtioMemXmlError::InvalidXml(error.to_string())),
            _ => {}
        }
        buffer.clear();
    }

    selected.ok_or_else(|| VirtioMemXmlError::AliasNotFound(expected_alias.to_owned()))
}

/// A device field's name and its index in the parsed values.
fn field_name(name: &[u8]) -> Option<(&'static str, usize)> {
    match name {
        b"size" => Some(("size", 0)),
        b"block" | b"block-size" => Some(("block", 1)),
        b"requested" => Some(("requested", 2)),
        b"current" => Some(("current", 3)),
        _ => None,
    }
}

fn bytes(value: Option<u64>, field: &'static str) -> Result<u64, VirtioMemXmlError> {
    value.ok_or(VirtioMemXmlError::MissingField(field))
}

fn attribute(
    element: &quick_xml::events::BytesStart<'_>,
    name: &[u8],
) -> Result<Option<String>, VirtioMemXmlError> {
    for attribute in element.attributes().with_checks(false) {
        let attribute =
            attribute.map_err(|error| VirtioMemXmlError::InvalidXml(error.to_string()))?;
        if attribute.key.as_ref() == name {
            let value = attribute
                .normalized_value(XmlVersion::Explicit1_0)
                .map_err(|error| VirtioMemXmlError::InvalidXml(error.to_string()))?;
            return Ok(Some(value.into_owned()));
        }
    }
    Ok(None)
}

fn compatibility_attribute(
    element: &quick_xml::events::BytesStart<'_>,
    dashed_name: &[u8],
    camel_name: &[u8],
) -> Result<CompatibilityEvidence, VirtioMemXmlError> {
    let value = attribute(element, dashed_name)?.or(attribute(element, camel_name)?);
    match value.as_deref().map(str::to_ascii_lowercase).as_deref() {
        None => Ok(CompatibilityEvidence::Unknown),
        Some("yes" | "on" | "true" | "1") => Ok(CompatibilityEvidence::Confirmed),
        Some("no" | "off" | "false" | "0") => Ok(CompatibilityEvidence::Rejected),
        Some(value) => Err(VirtioMemXmlError::InvalidValue {
            field: "compatibility attribute",
            value: value.to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_xml() -> &'static str {
        "<domain><memory model='virtio-mem'><target><size unit='MiB'>8</size><block unit='MiB'>2</block><requested unit='MiB'>4</requested><current unit='MiB'>4</current></target><alias name='ua-virtio-mem0'/></memory></domain>"
    }

    #[test]
    fn parses_alias_and_converts_units_to_bytes() {
        let parsed = parse_virtio_mem_xml(valid_xml()).expect("valid XML should parse");
        assert_eq!(parsed.alias, "ua-virtio-mem0");
        assert_eq!(parsed.memory.size_bytes, 8 * 1024 * 1024);
        assert_eq!(parsed.memory.block_size_bytes, 2 * 1024 * 1024);
        assert_eq!(parsed.compatibility, VirtioMemCompatibility::unknown());
    }

    #[test]
    fn a_value_without_a_unit_is_kib_as_libvirt_reads_it() {
        let xml = valid_xml().replace("<block unit='MiB'>2</block>", "<block>2048</block>");
        let parsed = parse_virtio_mem_xml(&xml).expect("valid XML should parse");
        assert_eq!(parsed.memory.block_size_bytes, 2 * 1024 * 1024);
    }

    #[test]
    fn parses_fully_unplugged_live_state() {
        let xml = valid_xml()
            .replace(
                "<requested unit='MiB'>4</requested>",
                "<requested unit='MiB'>0</requested>",
            )
            .replace(
                "<current unit='MiB'>4</current>",
                "<current unit='MiB'>0</current>",
            );
        let parsed = parse_virtio_mem_xml(&xml)
            .expect("zero requested/current is a valid fully unplugged state");

        assert_eq!(parsed.memory.requested_bytes, 0);
        assert_eq!(parsed.memory.current_bytes, 0);
    }

    #[test]
    fn rejects_unsupported_unit_and_overflow() {
        assert!(matches!(
            parse_virtio_mem_xml(&valid_xml().replace("unit='MiB'", "unit='parsec'")),
            Err(VirtioMemXmlError::UnsupportedUnit { .. })
        ));
        assert!(matches!(
            parse_virtio_mem_xml(&valid_xml().replace(
                "<size unit='MiB'>8</size>",
                "<size unit='GiB'>18446744000</size>"
            )),
            Err(VirtioMemXmlError::Overflow { field: "size" })
        ));
    }

    #[test]
    fn selects_only_the_requested_alias() {
        let xml = format!(
            "<domain>{}<memory model='virtio-mem'><target><size unit='MiB'>16</size><block unit='MiB'>2</block><requested unit='MiB'>8</requested><current unit='MiB'>8</current></target><alias name='ua-virtio-mem1'/></memory></domain>",
            valid_xml()
        );
        let selected = parse_virtio_mem_xml_for_alias(&xml, "ua-virtio-mem1")
            .expect("matching alias should parse");
        assert_eq!(selected.memory.size_bytes, 16 * 1024 * 1024);
        assert!(matches!(
            parse_virtio_mem_xml_for_alias(&xml, "missing"),
            Err(VirtioMemXmlError::AliasNotFound(_))
        ));
    }

    #[test]
    fn parses_compatibility_attributes_without_treating_absence_as_safe() {
        let xml = valid_xml().replace(
            "model='virtio-mem'",
            "model='virtio-mem' dynamic-memslots='on' unplugged-inaccessible='yes'",
        );
        let parsed = parse_virtio_mem_xml(&xml).expect("valid compatibility attributes");
        assert_eq!(
            parsed.compatibility.dynamic_memslots,
            CompatibilityEvidence::Confirmed
        );
        assert_eq!(
            parsed.compatibility.unplugged_inaccessible,
            CompatibilityEvidence::Confirmed
        );
        assert!(parsed
            .compatibility
            .merge(VirtioMemCompatibility::confirmed())
            .expect("external workload evidence should merge")
            .validate_for_resize()
            .is_ok());
    }
}
