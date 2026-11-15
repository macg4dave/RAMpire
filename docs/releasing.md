# Releasing

For the maintainer. How a release is made and published. There is no CI:
a release is built on the maintainer's workstation in the build container
([development](development.md#the-gate)), the same way as any development
build, and published to
[GitHub Releases](https://github.com/macg4dave/RAMpire/releases) with the
GitHub CLI. One script, `tools/release.sh`, does it.

## What a release is

- A version, `MAJOR.MINOR.PATCH`, that every crate has, and a tag
  `vVERSION` on the commit that sets it.
- Its entry in [CHANGELOG.md](../CHANGELOG.md), which is also its notes on
  GitHub.
- Its files, built from the clean, tagged tree and named for the version:

  | File | What |
  |---|---|
  | `rampire-VERSION-1.x86_64.rpm`, `rampire_VERSION_amd64.deb` | The host package, carrying the guest package of the same build |
  | `rampire-guest-VERSION-1.x86_64.rpm`, `rampire-guest_VERSION_amd64.deb` | The guest package, for installing a guest by hand |
  | `rampire-VERSION-x86_64-linux.tar.gz`, `rampire-guest-VERSION-x86_64-linux.tar.gz` | Static (musl) binaries for a host or guest without a package, with `INSTALL.md` |
  | `SHA256SUMS` | The checksum of each file above |

Releases are for Linux on x86-64. Windows guests come with milestone W,
and Arm packages later ([roadmap](roadmap.md#parked)). A `0.x` release is
published as a GitHub pre-release.

## Change notes

Every change someone using RAMpire would notice, a feature, a fix, a
changed default, a removed command, gets a line in `CHANGELOG.md` under
`## Unreleased` in the same commit. Write it for the person running
RAMpire: what changed for them, not how the code changed. Changes that
only touch the code, tests or build don't get a line.

If there is no `## Unreleased` section (just after a release), the first
such commit adds it under the introduction, above the newest release.
`## Unreleased: NAME` gives the coming release a name, for example
`## Unreleased: Linux only`.

## Making a release

From a clean checkout of `main`, with Docker or Podman and `gh` logged in
(`gh auth status`):

1. Prepare it:

   ```bash
   tools/release.sh prepare 0.5.0
   ```

   This sets every crate and `Cargo.lock` to the version, turns
   `## Unreleased` into `## 0.5.0 (DATE)` (keeping its name; `--name NAME`
   sets one), writes the new minor release's protocol fixtures
   (`fixtures/protocol/MAJOR.MINOR`, `tools/build.sh fixtures`), and
   commits `Release 0.5.0`. Nothing leaves the machine. Read the commit;
   fix anything with another commit.

2. Try the publish:

   ```bash
   tools/release.sh publish --dry-run
   ```

   It checks that `main` is clean and not behind `origin/main`, and that
   neither the tag nor the GitHub release exists yet. Then it runs the
   gate, tags `v0.5.0` here, builds the packages and tarballs from the tag
   (`tools/build.sh package --static`), installs them in clean Debian 13,
   Rocky Linux 10 and Fedora 45 containers (`tools/build.sh
   install-check`), checks `SHA256SUMS`, and prints the files and the
   notes it would publish.

3. Publish it:

   ```bash
   tools/release.sh publish
   ```

   The same steps, reusing the tag, then it pushes `main` and the tag and
   creates the GitHub release with the files attached.

`tools/release.sh notes VERSION` prints a release's notes: its CHANGELOG
entry, with links into the repository pointing at the tag, then how to
install it.

If a publish stops part way, fix the cause and run it again: it reuses a
tag that is on the same commit and refuses one on another. If the gate or
a build fails before anything was pushed, a fix is a new commit; move the
local tag onto it with `git tag -d v0.5.0` before publishing again. Once
pushed, a tag is never moved: release the fix as the next patch version.

## Versions

The version is the `rampire` crate's; `prepare` keeps the others the
same. A package built from anything other than the clean, tagged commit
gets a development version, `VERSION~dev.COUNT.BUILD`, which sorts before
the release ([development](development.md#installing-a-development-build));
a tag that doesn't match the crates' version is refused.

A controller serves guests from its own minor release and the one before
it. Each minor release keeps the documents its controller and guest
exchange in `fixtures/protocol/MAJOR.MINOR/`, and
`crates/rampire-core/tests/version_skew.rs` checks that today's code
still reads the previous minor release's.

## Not yet published

These pieces exist but nothing publishes them yet
([roadmap](roadmap.md#parked)):

| Piece | Tool |
|---|---|
| RPM signatures | `packaging/build.sh` signs every RPM with `RAMPIRE_SIGNING_KEY_ID` set |
| The source RPM for COPR | `packaging/srpm.sh VERSION BUILD_ID OUTPUT_DIRECTORY` |
| A signed apt repository from the `.deb`s | `packaging/apt-repo.sh PACKAGE_DIRECTORY OUTPUT_DIRECTORY`, signing with the only secret key in the GnuPG keyring |

Until then hosts install the release's package files directly
([quick start](quick-start.md#1-install)).
