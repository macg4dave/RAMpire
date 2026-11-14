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
| `rampire-host` | `host/` | Linux | The controller: settings resolution, runtime, libvirt, attestation, the CLI until O1.2 |
| `rampire-ops` | `crates/rampire-ops` | Linux | Operator views: controller status, the pool, one VM live |
| `rampire` | `rampire/` | Linux | The `rampire` binary: the daemon and operator commands |
| `rampire-guest-linux` | `guest-linux/` | Linux | Linux guest service and `rampire-workload` |
| `rampire-guest-windows` | `guest-windows/` | Windows (native) | Windows service and its workload |
| `xtask` | `tools/xtask` | Linux | Gate, deploy, observe, qualify (`cargo xtask`) |

`xtask` depends on the host and operations crates, so it builds only on Linux. The core
and shared guest crates build anywhere.

## The gate

Every change passes the gate before review. One command runs it from any
workstation with Docker or Podman, Windows and macOS included:

```bash
tools/build.sh                       # the gate
tools/build.sh package --static      # the packages, into dist/
tools/build.sh install-check         # install dist/ on clean Debian 13, Rocky 10 and Fedora 45
```

It checks whitespace in the working tree against `HEAD`, then runs
`cargo xtask gate checks` in the build container: `cargo fmt --check`, a
release build, all tests and Clippy with warnings denied for every
Linux-buildable crate, and `bash -n` on every shell script. On a Windows
workstation with Rust it also runs the Windows guest's tests and Clippy
natively.

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
units. It doesn't start anything: that is what the `testvm` scenarios are
for.

Two other builders, for a machine without a container engine:

| Builder | How |
|---|---|
| A Linux build host over SSH | `--host HOST`, or `RAMPIRE_BUILD_HOST` in the environment or `site/build.env` (copy [deploy/examples/build.env.example](../deploy/examples/build.env.example)), with `RAMPIRE_BUILD_DIR`, an absolute directory there the build owns. The host needs Rust with rustfmt and Clippy, and nfpm for packages |
| This machine | `--local`, on Linux with those tools installed |

The individual steps are `cargo xtask gate format|build|test|lint`.
`cargo xtask gate local` is `checks` plus `git diff --check`, for a Linux
checkout. `cargo xtask gate all` adds the native Windows gate below.

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

## Platform-specific checks

| Check | Where | Command |
|---|---|---|
| Host, xtask, Linux guest | The build container (`tools/build.sh`) or Linux | `cargo xtask gate checks` |
| Windows guest unit tests and Clippy | Windows workstation | part of `tools/build.sh` there |
| Windows release build, native tests, fetched binaries | Windows build VM | `make windows-native-gate` |
| Service control, sampling, ACLs, atomic replacement | A Windows guest | Deploy and observe; mocks do not prove them |
| Resizing, onlining, pressure, cache behaviour | A live VM under the controller | `cargo xtask metrics`, qualification |

The Windows targets in the `Makefile` read the site's build VM from
`site/windows-build.mk` (copy
[windows-build.mk.example](../deploy/examples/windows-build.mk.example)),
or from the file named by `WINDOWS_SITE`. `make windows-native-gate` checks
the toolchain, synchronises the working tree, release-builds, runs the
native tests, `fmt` and Clippy, then fetches the binaries and records them
in `build.json`.

## Build identity

A deployed binary names the exact source it came from. The identity is the
commit (`<12 hex>`), or for a working tree that differs from it
`<commit>-dirty.<12 hex of the source digest>`: a SHA-256 over every
tracked and untracked, non-ignored file. The build compiles it in through
`RAMPIRE_BUILD_ID`; a build without it reports `unversioned`.

- Linux guest: `cargo xtask linux build` builds the release binary and
  checks that `rampire-guest version` reports the checkout's identity.
- Windows guest: `sync` stamps the remote workspace, `build` compiles the
  stamp in, `fetch` verifies the binaries by SHA-256 and writes
  `build.json`. `make windows-status` compares the installed service with
  that record.

Every guest publication carries its build identity, so `watch`, `metrics`
and captures show which build produced each sample.

## Development guests

`cargo xtask linux deploy SSH_DESTINATION --output PATH --apply` installs the
stamped Linux guest, its unit, the RAM monitor and the workload on a
development VM over SSH (the account needs passwordless `sudo`). It runs on
a Linux checkout that can reach the guest. Without `--apply` it records
what it would do. `status` and `uninstall` complete it; see
[tools](tools.md). Hosts and guests that aren't under development
install packages, built with `cargo xtask package`.

Windows deployment is `make windows-deploy` with the site's VM settings.

## Live tests

Live tests change real VMs and the host's memory. They need the owner's
authorisation, a bounded duration and bounded memory. Record them with
`cargo xtask metrics` (or `qualification`) so the result is evidence, not
recollection, and open `tools/linux/watch-ram.sh` (Linux guest) or
`tools/windows/Watch-Ram.ps1` (Windows guest) to watch them. Results go in
the roadmap with the capture's path under `.artifacts/`.

Every VM a test needs is made and removed with `cargo xtask testvm`
([tools](tools.md#test-vms)). A test that needs a VM no scenario
describes gets a new scenario in `tools/testvm`, not a VM made by hand.

## Site configuration

Nothing site-specific is tracked. VM names, hosts, SSH aliases, paths and
pool sizes live in a site's own files: on a host, its installed
configuration; in a checkout, the untracked `site/` directory. The tracked
[deploy/examples](../deploy/examples) are neutral templates.

## Installing a development build

A development host installs the same packages as any other host, built
from the checkout:

```bash
tools/build.sh package                               # into dist/, in the build container
sudo dnf install ./dist/rampire-*.x86_64.rpm         # or: sudo apt install ./dist/rampire_*_amd64.deb
sudo rampire init                                    # a new host; an existing one keeps its configuration
```

`package` stamps the host and guest binaries with the checkout's build
identity (`<commit>`, or `<commit>-dirty.<source digest>`), which
`rampire --version` reports, and builds the RPMs and `.deb`s with nfpm
(`packaging/build.sh`). A development version is `VERSION~dev.BUILD`, so
it sorts before the release and an upgrade to that release replaces it.
`--static` adds the musl tarballs a release ships. The guest packages
(`rampire-guest`) are in `dist/` too.

Upgrading a running host is the package manager's upgrade: the package
restarts a running controller onto the new binary.
