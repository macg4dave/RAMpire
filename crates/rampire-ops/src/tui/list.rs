//! How the VM list is shown: its filter, its order, what its last columns
//! hold, and the VMs marked for an action on several.

use std::collections::BTreeSet;

use super::scroll::Scroll;

/// The list's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortBy {
    /// The managed VMs as the pool lists them, then the others by kind.
    #[default]
    Pool,
    Name,
    /// The RAM plugged into the device.
    Plugged,
    Priority,
    /// The share of its memory the guest uses.
    GuestUse,
}

impl SortBy {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pool => "pool order",
            Self::Name => "name",
            Self::Plugged => "RAM plugged",
            Self::Priority => "priority",
            Self::GuestUse => "guest in use",
        }
    }
}

/// What the list's last columns show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Linemode {
    /// The guest's memory in use, and its request.
    #[default]
    Guest,
    /// min_ram and max_ram.
    Limits,
    /// The device: requested, plugged and its size.
    Device,
    /// What `vm list` says about the VM.
    Detail,
}

impl Linemode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Guest => "guest",
            Self::Limits => "limits",
            Self::Device => "device",
            Self::Detail => "detail",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListState {
    pub scroll: Scroll,
    /// Only the VMs whose names contain it, case aside.
    pub filter: Option<String>,
    pub sort: SortBy,
    pub reverse: bool,
    pub linemode: Linemode,
    /// By name, so they stay marked however the list moves.
    pub marks: BTreeSet<String>,
}

impl ListState {
    /// Whether `name` passes the filter.
    pub fn shows(&self, name: &str) -> bool {
        self.filter
            .as_deref()
            .is_none_or(|filter| name.to_lowercase().contains(&filter.to_lowercase()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_filter_matches_names_whatever_their_case() {
        let mut list = ListState::default();
        assert!(list.shows("Web1"));
        list.filter = Some("WEB".to_owned());
        assert!(list.shows("web1"));
        assert!(!list.shows("db1"));
    }
}
