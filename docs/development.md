# Development

For developers. How to build, check and test this repository, and which checks need which
platform. Open work is in [the roadmap](roadmap.md);
[the Rust standards](../.github/prompts/rust.md) hold coding rules;
[administration](administration.md) covers installing and running the
product. The rule for importing third-party code is in the previous
roadmap (`git show 71f1de7~1:docs/roadmap.md`, "Importing third-party
code").

## Workspace

| Crate | Path | Builds on | Owns |
|---|---|---|---|
| `rampire-core` | `crates/rampire-core` | any | Policy, request protocol, wire and settings formats |
| `rampire-guest` | `crates/rampire-guest` | any | Guest request store, worker, transports, service loop |
| `rampire-host` | `host/` | Linux | The controller: settings resolution, runtime, libvirt, attestation |
| `rampire-ops` | `crates/rampire-ops` | Linux | What the commands do: the guided flows behind one question interface (`ui::Ui`, `flows/`), the reports, the doctor, the readings of the pool and one VM, and the TUI (`tui/`) |
| `rampire` | `rampire/` | Linux | The `rampire` binary: the daemon and operator commands |
| `rampire-guest-linux` | `guest-linux/` | Linux | Linux guest service |
| `rampire-guest-windows` | `guest-windows/` | Windows (native) | Windows service and its workload |
| `xtask` | `tools/xtask` | Linux | The gate and the packages (`cargo xtask`) |

`xtask` depends on the host and operations crates, so it builds only on Linux. The core
and shared guest crates build anywhere.

## The gate

Every change passes the gate before review. One command runs it from any
workstation with Docker or Podman, Windows and macOS included:

```bash
tools/build.sh                       # the gate, then the packages into dist/
tools/build.sh check                 # the gate alone
tools/build.sh package --static      # the packages alone, into dist/
tools/build.sh install-check         # install dist/ on clean Debian 13, Rocky 10 and Fedora 45
```

It checks whitespace in the working tree against `HEAD`, then runs
`cargo xtask gate checks` in the build container: `cargo fmt --check`, a
release build, all tests and Clippy with warnings denied for every
Linux-buildable crate, and `bash -n` on every shell script. On a Windows
workstation with Rust it also runs the Windows guest's tests and Clippy
natively.

If Podman says `default OCI runtime "crun" not found` although crun is
installed, its rootless database still names another user ID's
`/run/user/UID` (a home folder copied from another machine). Move
`~/.local/share/containers/storage/db.sql` aside; Podman writes a new one.
`podman --log-level=debug info` shows the folder it can't create.

The build container is
[tools/container/Containerfile](../tools/container/Containerfile): Rust,
rustfmt, Clippy, the musl target, nfpm and rpm at fixed versions. It is
built on first use and again whenever that file changes; Cargo's registry
and `target/` stay in the volumes `rampire-cargo` and `rampire-target`.
The first run downloads the Rust image and compiles everything, about ten
minutes; later runs take seconds to a few minutes. `docker volume rm
rampire-cargo rampire-target` starts it cold again.
The container gets the working tree as it is, uncommitted and untracked
(not ignored) files included, with `.git`, so a package built there is
stamped with the checkout's build identity. Nothing else on the
workstation is visible to it.

`install-check` installs the host and guest packages from `dist/` in a
clean container of each family and checks that each installs with its
dependencies, reports the build, and leaves the service account and both
units. It doesn't start anything.

Two other builders, for a machine without a container engine:

| Builder | How |
|---|---|
| A Linux build host over SSH | `--host HOST`, or `RAMPIRE_BUILD_HOST` in the environment or `site/build.env` (copy [deploy/examples/build.env.example](../deploy/examples/build.env.example)), with `RAMPIRE_BUILD_DIR`, an absolute directory there the build owns. The host needs Rust with rustfmt and Clippy, and nfpm for packages |
| This machine | `--local`, on Linux with those tools installed |

The individual steps are `cargo xtask gate format|build|test|lint`.
`cargo xtask gate local` is `checks` plus `git diff --check`, for a Linux
checkout.

### Checking one crate

```bash
cargo test -p rampire-core          # anywhere
cargo test -p rampire-guest         # anywhere
cargo test -p rampire-guest-windows # Windows
cargo test -p rampire-host     # Linux
```

Unit tests read only [fixtures](../fixtures/config) and temporary files.
None needs a VM, a running service or a site's configuration. The host's
`documented_examples_load` test also loads
[deploy/examples](../deploy/examples), so the examples stay valid.

### The TUI and its tests

The TUI is drawn with [Ratatui](https://ratatui.rs) 0.30 on its
`crossterm` backend, both MIT-licensed crates used as dependencies.
`rampire top` and `rampire watch` draw the TUI's dashboard and VM details
alone, so there is one renderer.

It is laid out as [Yazi](https://github.com/sxyazi/yazi)'s is, some of it
with Yazi's code (MIT, [THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md)).
The module list is in `tui/mod.rs`'s documentation; in short:

- `tui/keymap.rs` is the one table of keys: each binding's layer, keys,
  command, description and status-bar hint. Dispatch, the help, the
  status bar, the which popup of a chord and the command palette all read
  it, so a new key is one line there and a match arm in `tui/router.rs`.
- `tui/router.rs` sends a key to the topmost layer (`App::layer`): a
  text field first, then that layer's keys, then the keys of every list
  and of anywhere; then does the command in that layer.
- `tui/app.rs` holds the state and what is derived from it; `tui/views/`
  draws it, the layers from the bottom up; `App::effects` carries what
  only the screen's loop can do (copying, Ctrl+Z).
- An action runs as a task (`tui/tasks.rs`), one at a time, its flow on
  its own thread. The loop in `tui/mod.rs` sleeps until a message (a key
  from the key thread, a reading, a task's line) or the time a notice
  moves, and draws at most once a frame.

A guided flow (`vm add`, `set`, `apply`, ...) talks to the person only
through `rampire_ops::ui::Ui`: `ui::Terminal` for the commands, and the
TUI's tasks through a channel (`tui/jobs.rs`). Tests answer it with
`ui::Transcript`. TUI tests set `App::hold_tasks`, so a task is queued but
its command never runs.

What each control does, from the key to the flow it runs, with how each
is tested and what is verified only on a deployment, is in
[the TUI audit](tui-functional-audit.md); the workflows a person
follows, in [TUI workflows](tui-workflows.md).

The screenshot software centres show, `docs/screenshots/tui.png`, is the
dashboard drawn by the same renderer from a made-up host
(`tui/screenshot.rs`). After a change to the dashboard, draw it again:

```bash
cargo test -p rampire-ops -- --ignored tui::screenshot
rsvg-convert target/screenshots/tui.svg -o docs/screenshots/tui.png
```

Each screen and layer is drawn in tests into a buffer at 80x24, 120x40
and 200x60, in every theme, at every colour depth and in both icon sets
(`tui/views/mod.rs`), as part of the gate; keys are pressed through
`App::on_key` as a person would (`tui/router.rs`).

### Third-party licenses

`cargo xtask licenses DIR`, run by `cargo xtask package`, writes the
license and notice files of every crate each binary is built from, which
the packages install as `THIRD_PARTY_LICENSES`. A crate with no license
file stops the build. Adding a dependency needs nothing more; copying
code does: [THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md).

### Man pages and completions

`packaging/docs.sh`, run by the package build, makes them from the
binaries just built: `rampire generate man|completions` (hidden) writes
`rampire(8)`, a page per subcommand and `rampire.toml(5)` from clap's
definitions and `rampire_core::settings_reference`, and the guest's page
wraps its own help. A test holds docs/reference.md's settings
tables to `settings_reference`, and that to the settings the configuration
reads.

## Platform-specific checks

| Check | Where | Command |
|---|---|---|
| Host, xtask, Linux guest | The build container (`tools/build.sh`) or Linux | `cargo xtask gate checks` |
| Windows guest unit tests and Clippy | Windows workstation | part of `tools/build.sh` there |
| Resizing, onlining, pressure, cache behaviour | A deployment: the packages on a real host and guest | `rampire watch`, `rampire top`, `rampire doctor VM` |

The Windows guest is parked. Its build and deployment come back with it,
as a package, not as development scripts.

## Build identity

A deployed binary names the exact source it came from. The identity is the
commit (`<12 hex>`), or for a working tree that differs from it
`<commit>-dirty.<12 hex of the source digest>`: a SHA-256 over every
tracked and untracked, non-ignored file. The build compiles it in through
`RAMPIRE_BUILD_ID`; a build without it reports `unversioned`.

`cargo xtask package` stamps both binaries, so `rampire --version` and
`rampire-guest version` name the build. Every guest publication carries
its build identity, so `watch` and `top` show which build produced each
sample.

## Deployments

Live work is done on deployments: the packages installed on a real host
and guest, and the product's own commands (`rampire init`, `rampire vm
add`, `rampire watch`, `rampire top`). A deployment is kept in place and
improved as findings are fixed. Nothing outside the product makes,
deploys or tests VMs; a step the product can't do is a missing product
feature.

## Site configuration

Nothing site-specific is tracked. VM names, hosts, SSH aliases, paths and
pool sizes live in a site's own files: on a host, its installed
configuration; in a checkout, the untracked `site/` directory. The tracked
[deploy/examples](../deploy/examples) are neutral templates.

## Installing a development build

A development host installs the same packages as any other host, built
from the checkout:

```bash
tools/build.sh install              # build in the container, install or upgrade with dnf or apt
sudo rampire init                   # a new host; an existing one keeps its configuration
```

`package` stamps the host and guest binaries with the checkout's build
identity (`<commit>`, or `<commit>-dirty.<source digest>`), which
`rampire --version` reports, and builds the RPMs and `.deb`s with nfpm
(`packaging/build.sh`). A development version is `VERSION~dev.COUNT.BUILD`, COUNT being the
commits to `HEAD`, so a later commit's package always upgrades an earlier
one, and the release sorts after every development version that led to
it. `install` checks that the installed `rampire --version` reports the
build just made.
The guest is also built for musl every time: the host package carries
that static binary for guests without dnf or apt, so a `--local` build
needs the `x86_64-unknown-linux-musl` target (the build container has
it). `--static` adds the musl host binary and the tarballs a release
ships. The guest packages (`rampire-guest`) are in `dist/` too.

## Releases and change notes

A change someone using RAMpire would notice gets a line under
`## Unreleased` in [CHANGELOG.md](../CHANGELOG.md), in the same commit.
`tools/release.sh` makes the release from it:
[releasing](releasing.md).

Upgrading a running host is the package manager's upgrade: the package
restarts a running controller onto the new binary.
