# Changelog

Every release of RAMpire, newest first. Versions follow semantic
versioning. Upgrade the controller first, then the guests: a controller
serves guests from its own minor release and the one before it.

## Unreleased: 0.1.0

The first release under the name RAMpire. Nothing is migrated from the
earlier virtio-mem pool controller: remove an old install and install
again (decision D9).

- A TUI: `sudo rampire` on a terminal, or `ssh -t HOST sudo rampire`
  from any computer. The pool and each VM live; adding, removing and
  bringing back VMs; changing a VM's settings; the doctor, with fixes it
  can run; the journal by VM. Every action shows the command that does
  the same and is logged to the journal (`event=operator_change`).
  `rampire top` and `rampire watch` draw its screens alone.
- `rampire vm list` and `rampire doctor VM`.
- Man pages, `man rampire`, `man rampire-COMMAND` and `man rampire.toml`,
  and `rampire-guest(8)` and `rampire-watch-ram(1)` in the guest package,
  made from the same definitions as `--help`; shell completion for bash,
  zsh and fish. Options meant for scripts are under "For scripts" in
  `--help` and the man pages, and docs/fleet.md says what stays the same
  between releases.
- One command, `rampire`: `init`, `status`, `top`, `watch`, `check`,
  `diff`, `apply`, `rollback`, `set`, `logs`, `doctor` and `recover`, and
  `daemon` for the unit. Every command takes `--json` and `--yes`, and
  exit codes are documented.
- The controller runs only a copy of the configuration that
  `rampire apply` checked, so a mistake in `/etc/rampire` never reaches
  it. Each apply is recorded under `/var/lib/rampire/evidence`.
- Configuration is `global.toml` and one `vms.d/NAME.toml` per VM. Review
  profiles are TOML, with a source for every answer.
- One unit, `rampire.service`, ordered after libvirt's single or split
  daemons.
- `rampire init` checks the host first, and leaves the controller
  enabled and running. `rampire vm add --fix-xml` also adds a missing NUMA
  cell and `maxMemory`, and reads the guest's kernel release from its QEMU
  guest agent. `rampire status` shows the controller unit's state.
- Host and guest packages as RPM and `.deb`, and static tarballs. The
  guest package recommends `qemu-guest-agent`.
- Every document names its schema and starts at version 1. The control
  channel port is `org.rampire.control.0`.
