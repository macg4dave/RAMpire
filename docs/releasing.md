# Releasing

How a release is built. There is no CI: a release is built in the build
container ([development](development.md#the-gate)), the same way as any
development build.

## Each release

1. Set the version in `rampire/Cargo.toml` (and the other crates), and
   add its entry to `CHANGELOG.md`.
2. Run the gate and commit:

   ```bash
   tools/build.sh
   ```

3. Tag the commit, then build from the clean, tagged tree:

   ```bash
   git tag v0.1.0
   tools/build.sh package --static
   ```

   `dist/` then holds the host and guest RPMs and `.deb`s, the static
   tarballs and `SHA256SUMS`, all named for the release version
   (`0.1.0`). A tree that isn't clean, or a tag that doesn't match the
   crate's version, gives a development version instead
   (`0.1.0~dev.BUILD`) or is refused.
4. `packaging/srpm.sh VERSION BUILD_ID OUTPUT_DIRECTORY` makes the source
   RPM a COPR project builds from.

## Publishing

Not decided since CI was removed; see the
[roadmap](roadmap.md#after-this-roadmap). The pieces that exist:

| Piece | Tool |
|---|---|
| The packages and checksums | `tools/build.sh package --static` |
| The source RPM for COPR | `packaging/srpm.sh` |
| A signed apt repository from the `.deb`s | `packaging/apt-repo.sh PACKAGE_DIRECTORY OUTPUT_DIRECTORY`, signing with the only secret key in the GnuPG keyring |

Until a repository is published, hosts and guests install the package
files directly (`dnf install ./rampire-VERSION.x86_64.rpm`,
`apt install ./rampire_VERSION_amd64.deb`).
