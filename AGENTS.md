# Repository Agent Instructions

Project documentation starts at [docs/](docs/).
Task prompts are in `.github/prompts/`.

## Operate through the product

The project is a product, not an early prototype. Anything done to a
virtual machine, a guest, the host or the controller is done with the
product itself: the `rampire` and `rampire-guest` packages and their
command, `rampire`. That covers installing,
setting up, adding, configuring, starting, stopping, resizing and
observing VMs.

- There is no separate test tooling. `cargo xtask` and `tools/build.sh`
  only build the product (the gate and the packages); they never make,
  deploy to or operate VMs.
- If the product can't do a task, that is a missing feature: add it to
  `rampire` (a subcommand or an option), document it, then use it. Do not
  do the job by hand in the meantime, and do not add a script beside the
  product to do it.
- If a task takes several commands that always go together, combine them
  into one and update the docs.
- Do not write one-off scripts, ad hoc `ssh`/`virsh`/`qemu` command chains
  or throwaway helpers to operate the product, in the repository or in a
  scratch directory.
- If a command is broken or gives the wrong result, fix it rather than
  working around it.
- Deployments are kept in place and improved, not torn down after a run.

## Ask before installing on a host or guest

Do not install, upgrade or start RAMpire's host or guest packages or
services on any machine without the user's permission first. Ask each
time; permission for one install does not cover the next.

This is only about putting the product on a machine. Building and
testing the build need no permission: `cargo build`, `cargo test`,
`cargo xtask gate ...`, `cargo xtask package`, `tools/build.sh` without
`install` (including `tools/build.sh package` and the build container),
`tools/build.sh install-check` (it installs only into throwaway
containers), and running the built binary from `target/` without
installing it.

Once the user agrees, install the current build: in the checkout, at the
commit under test,

```bash
tools/build.sh install
```

builds the packages in the build container and installs the host package
with dnf or apt, upgrading what is installed; it checks that `rampire
--version` reports the build just made. A guest gets the current guest
package from the same build (`tools/build.sh package`).

These rules are about operating the product. It does not restrict editing,
building or testing the code in this repository.
