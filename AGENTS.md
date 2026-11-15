# Instructions for AI agents

RAMpire's documentation starts at [docs/](docs/): what it does in
[usage](docs/usage.md), how it is built and checked in
[development](docs/development.md), how it is tested in
[testing](docs/testing.md), and what is left in the
[roadmap](docs/roadmap.md). Coding rules are in
[the Rust standards](.github/prompts/rust.md).

## Finishing a change

- The gate passes: `cargo xtask gate local` (or `tools/build.sh check`).
- A change a user would notice has a line under `## Unreleased` in
  [CHANGELOG.md](CHANGELOG.md), and the docs that describe it say what it
  does now.
- A roadmap item that is done is taken out of the roadmap, not marked
  done.
- Say what was verified and what wasn't. Code that passed unit tests but
  was never run against a real host is not "fixed on the host".

## Bugs and tests

Every bug found, by a person or by a test, gets a test that fails without
the fix, at the lowest layer that can show it ([testing](docs/testing.md)):

1. a unit test beside the code;
2. the simulated host, for anything a person does through a command or
   the TUI: whole workflows, with a fake libvirt, guest agent and journal
   that can fail the way real ones do. It runs in the gate;
3. the live suite, for what only a real VM shows (a guest service, the
   control channel, a restart). It makes its own `rampire-test-*` VMs on
   this host.

Before saying a TUI or flow change works, run it through layer 2, and
through layer 3 when it touches a guest, libvirt or the controller.

## Operating VMs and hosts

- Use the product (`rampire`) to do what it does: set up, add, change,
  restart, upgrade, remove, observe. When it can't do something a person
  would need, that is a missing feature: add it, rather than doing the job
  by hand with `virsh` or `ssh`.
- When a `rampire` command gives a wrong result, fix the command.
- Test code may create, break and delete its own `rampire-test-*` VMs with
  libvirt directly: making VMs is not something the product does.

## Installing

Never install, upgrade or start RAMpire on a machine, or change a VM that
isn't a `rampire-test-*` VM, without asking the user first, each time.

These need no permission:

- building and testing the build: `cargo build`, `cargo test`,
  `cargo xtask gate ...`, `cargo xtask package`, `tools/build.sh` without
  `install`, `tools/build.sh install-check` (throwaway containers), and
  running binaries from `target/`;
- the live suite, `cargo xtask live`, on this host: it installs the
  current build here with `tools/build.sh install` and works only on its
  own `rampire-test-*` VMs.
