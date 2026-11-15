# Third-party notices

RAMpire is GPL-3.0-or-later ([LICENSE](LICENSE)).

## Code built into the binaries

`rampire` and `rampire-guest` are built from Rust crates under their own
licenses: MIT, Apache-2.0, BSD and others compatible with the GPL, among
them [Ratatui](https://ratatui.rs) and crossterm (MIT) for the TUI. No
crate's code is copied into this repository; they are dependencies, listed
in [Cargo.lock](Cargo.lock).

Their licenses ask that their notices go with the binaries. The package
build (`cargo xtask licenses`, run by `cargo xtask package`) collects the
license and notice files of every crate each binary is built from, and the
packages install them:

| Package | File |
|---|---|
| `rampire` | `/usr/share/doc/rampire/THIRD_PARTY_LICENSES` |
| `rampire-guest` | `/usr/share/doc/rampire-guest/THIRD_PARTY_LICENSES` |

A crate that ships no license file stops the build, so a package never
goes out without a notice it needs.

## Ideas from other projects

Ideas, unlike code, carry no license; these are named for credit. No code
is copied for them.

| Project | License | What RAMpire's TUI takes from it |
|---|---|---|
| [Yazi](https://github.com/sxyazi/yazi) at commit `d5e4beb6b5212c28d1026d8e7d4ae985dcf174cb` | MIT | Yes and No buttons each centred in half of a confirmation, which also lists exactly what it will do; one key table per layer that dispatch, help and hints are made from, layers taking keys topmost first and drawn bottom first; actions as background tasks, with a gauge in the status bar and a list of them whose failed ones open on their output; a spotter of a VM's facts with a cell to copy; a preview of the chosen VM beside the list; copying with OSC 52; a filter and a find with `n` and `N`; the list's last columns chosen by a chord (its linemodes), sorting by a chord, marking several VMs for one action; popups placed beside the row they are about; synchronized output; drawing only when something changes, at most once a frame; asking the terminal what it can show; Ctrl+Z; and from its data distribution service, telling one TUI of the changes made from another |
| [Posting](https://github.com/darrenburns/posting) by Darren Burns, at commit `32c45669032342cf9b95d5758af47a0d93de0ad7` | Apache-2.0 | A command palette found by typing a few letters, with each command's key beside it and themes previewed as the cursor moves over them; yes-or-no questions shown as buttons |

## Code copied from other projects

Each file that holds copied code keeps the project's copyright line, adds
ours, names its license with an `SPDX-License-Identifier` line and says
what was changed. The license text is in [LICENSES](LICENSES), and the
package of the binary the code is built into installs the notice in its
`THIRD_PARTY_LICENSES` (`COPIED` in `tools/xtask/src/licenses.rs`).

| Project | Commit | License | Copyright | Our files | What |
|---|---|---|---|---|---|
| [Yazi](https://github.com/sxyazi/yazi) | `d5e4beb6b5212c28d1026d8e7d4ae985dcf174cb` | MIT, [LICENSES/MIT.txt](LICENSES/MIT.txt) | Copyright (c) 2023 - sxyazi | `crates/rampire-ops/src/tui/notify.rs` | Its notifications: boxes that slide in at the bottom right, as wide as their text, stacked as many as fit (from `yazi-fm/src/notify/notify.rs`, `yazi-core/src/notify/`, `yazi-actor/src/notify/tick.rs`) |
| [Yazi](https://github.com/sxyazi/yazi) | `d5e4beb6b5212c28d1026d8e7d4ae985dcf174cb` | MIT, [LICENSES/MIT.txt](LICENSES/MIT.txt) | Copyright (c) 2023 - sxyazi | `crates/rampire-ops/src/tui/scroll.rs` | Moving through a list: a cursor kept a margin from the view's edges, moved a row, by a share of a page or to either end (from `yazi-widgets/src/scrollable.rs`, `yazi-widgets/src/step.rs`) |
| [Yazi](https://github.com/sxyazi/yazi) | `d5e4beb6b5212c28d1026d8e7d4ae985dcf174cb` | MIT, [LICENSES/MIT.txt](LICENSES/MIT.txt) | Copyright (c) 2023 - sxyazi | `crates/rampire-ops/src/tui/which.rs` | The popup of what can follow a key: narrowing the candidates as keys are typed, and its layout at the bottom in up to three columns (from `yazi-core/src/which/which.rs`, `yazi-fm/src/which/`) |
| [Yazi](https://github.com/sxyazi/yazi) | `d5e4beb6b5212c28d1026d8e7d4ae985dcf174cb` | MIT, [LICENSES/MIT.txt](LICENSES/MIT.txt) | Copyright (c) 2023 - sxyazi | `crates/rampire-ops/src/tui/help.rs` | Finding keys in the help by typing, a key itself before its description, and the help's layout (from `yazi-core/src/help/help.rs`, `yazi-fm/src/help/help.rs`) |
| [Yazi](https://github.com/sxyazi/yazi) | `d5e4beb6b5212c28d1026d8e7d4ae985dcf174cb` | MIT, [LICENSES/MIT.txt](LICENSES/MIT.txt) | Copyright (c) 2023 - sxyazi | `crates/rampire-ops/src/tui/input.rs` | A text field's earlier answers, by question, the newest twenty (from `yazi-core/src/input/history.rs`) |
