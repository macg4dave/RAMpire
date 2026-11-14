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
| `rampire-guest-linux` | `guest-linux/` | Linux | Linux guest service |
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

## Platform-specific checks

| Check | Where | Command |
|---|---|---|
| Host, xtask, Linux guest | The build container (`tools/build.sh`) or Linux | `cargo xtask gate checks` |
| Windows guest unit tests and Clippy | Windows workstation | part of `tools/build.sh` there |
| Resizing, onlining, pressure, cache behaviour | A deployment: the packages on a real host and guest | `rampire watch`, `rampire top`, `rampire-watch-ram` |

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
`--static` adds the musl tarballs a release ships. The guest packages
(`rampire-guest`) are in `dist/` too.

Upgrading a running host is the package manager's upgrade: the package
restarts a running controller onto the new binary.
