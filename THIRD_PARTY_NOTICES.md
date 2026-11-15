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
is copied from them.

| Project | License | What RAMpire's TUI takes from it |
|---|---|---|
| [Posting](https://github.com/darrenburns/posting) by Darren Burns, at commit `32c45669032342cf9b95d5758af47a0d93de0ad7` | Apache-2.0 | A command palette found by typing a few letters, with each command's key beside it and themes previewed as the cursor moves over them; short notices (toasts) in a corner instead of a status line; yes-or-no questions shown as buttons |

Code copied from another project, should any be, is recorded here with
its project, commit, license and copyright holders, and the files that
hold it; its license text goes in `LICENSES/`.
