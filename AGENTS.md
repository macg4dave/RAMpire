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

## Test hosts run the current build

Before testing on a development or test host, upgrade RAMpire there to the
current build, without asking: in its checkout, at the commit under test,

```bash
tools/build.sh install
```

builds the packages in the build container and installs the host package
with dnf or apt, upgrading what is installed; it checks that `rampire
--version` reports the build just made. A guest gets the current guest
package from the same build (`tools/build.sh package`).

This rule is about operating the product. It does not restrict editing,
building or testing the code in this repository.
