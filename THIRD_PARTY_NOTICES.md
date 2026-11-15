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
| [Yazi](https://github.com/sxyazi/yazi) at commit `d5e4beb6b5212c28d1026d8e7d4ae985dcf174cb` | MIT | Yes and No buttons each centred in half of a confirmation |
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
