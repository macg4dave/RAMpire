//! RHEL host-side adapters for guest-driven virtio-mem resizing.

pub mod attestation;
pub mod checkpoint;
pub mod compatibility_source;
pub mod config;
#[cfg(unix)]
pub mod control_channel;
pub mod files;
pub mod guest_channel;
pub mod host_memory;
#[cfg(unix)]
pub mod libvirt;
pub mod metrics;
pub mod operator_actions;
pub mod ownership;
pub mod qga;
pub mod resize_sink;
pub mod runtime;
pub mod virsh;
pub mod xml_source;

#[cfg(unix)]
pub mod wake;
