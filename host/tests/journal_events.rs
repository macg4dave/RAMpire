//! Every journal event the controller logs is listed in docs/journal.md
//! (docs/fleet.md, the script interface), so scripts can rely on the names.

use std::collections::BTreeSet;
use std::path::Path;

fn events_in(text: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for (index, _) in text.match_indices("event=") {
        let name: String = text[index + "event=".len()..]
            .chars()
            .take_while(|c| c.is_ascii_lowercase() || *c == '_')
            .collect();
        if !name.is_empty() {
            found.insert(name);
        }
    }
    found
}

/// Every event logged by the Rust sources under `directory`, nested modules
/// included.
fn events_under(directory: &Path, found: &mut BTreeSet<String>) {
    for entry in std::fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            events_under(&path, found);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            found.extend(events_in(&std::fs::read_to_string(&path).unwrap()));
        }
    }
}

#[test]
fn every_logged_event_is_documented() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut logged = BTreeSet::new();
    for directory in ["host/src", "rampire/src"] {
        events_under(&root.join(directory), &mut logged);
    }
    // People's changes, logged by the operator commands and the TUI.
    logged.extend(events_in(
        &std::fs::read_to_string(root.join("crates/rampire-ops/src/audit.rs")).unwrap(),
    ));
    let documented = std::fs::read_to_string(root.join("docs/journal.md")).unwrap();
    let missing: Vec<_> = logged
        .iter()
        .filter(|event| !documented.contains(&format!("`{event}`")))
        .collect();
    assert!(logged.len() > 10, "found only {logged:?}");
    assert!(
        missing.is_empty(),
        "add these to docs/journal.md: {missing:?}"
    );
}
