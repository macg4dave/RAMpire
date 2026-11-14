//! The controller's systemd unit: one per host (decision D2).

/// The controller's unit.
pub const CONTROLLER_UNIT: &str = "rampire.service";

/// Refuse any unit but the controller's.
pub fn require_controller_unit(unit: &str) -> Result<(), String> {
    if unit == CONTROLLER_UNIT {
        Ok(())
    } else {
        Err(format!(
            "{unit} is not the controller unit {CONTROLLER_UNIT}"
        ))
    }
}

/// The controller unit's state from `systemctl show`. A field systemd
/// doesn't report reads as `unknown`.
pub fn controller_state() -> crate::reports::ControllerUnit {
    let text = crate::process::bounded_text(
        "systemctl",
        &[
            crate::process::os("show"),
            crate::process::os("--property=ActiveState,SubState,UnitFileState"),
            crate::process::os(CONTROLLER_UNIT),
        ],
        std::path::Path::new("/"),
        std::time::Duration::from_secs(10),
    )
    .unwrap_or_default();
    parse_show(&text)
}

fn parse_show(text: &str) -> crate::reports::ControllerUnit {
    let field = |key: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
            .filter(|value| !value.is_empty())
            .unwrap_or("unknown")
            .to_owned()
    };
    crate::reports::ControllerUnit {
        unit: CONTROLLER_UNIT.to_owned(),
        active_state: field("ActiveState"),
        sub_state: field("SubState"),
        unit_file_state: field("UnitFileState"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn show_output_is_read_and_gaps_are_unknown() {
        let unit = parse_show("ActiveState=active\nSubState=running\nUnitFileState=\n");
        assert_eq!(unit.active_state, "active");
        assert_eq!(unit.sub_state, "running");
        assert_eq!(unit.unit_file_state, "unknown");
        assert_eq!(parse_show("").active_state, "unknown");
    }

    #[test]
    fn only_the_controller_unit_is_accepted() {
        assert_eq!(require_controller_unit("rampire.service"), Ok(()));
        for unit in ["rampire@main.service", "other.service", ""] {
            assert!(require_controller_unit(unit).is_err(), "{unit}");
        }
    }
}
