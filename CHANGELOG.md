# Changelog

Every release of RAMpire, newest first. Versions follow semantic
versioning. Upgrade the controller first, then the guests: a controller
serves guests from its own minor release and the one before it.

A change someone using RAMpire would notice gets a line under
"Unreleased" in the commit that makes it. `tools/release.sh prepare`
turns that section into the release's entry, and the entry becomes the
release's notes on GitHub ([releasing](docs/releasing.md)).

## Unreleased

- The TUI sets up a host that isn't set up yet: it opens **Set up this
  host** first, with the recommended host reserve filled in to keep or
  change and the pool it leaves shown as you type. `rampire init` asks
  the reserve the same way, and `--host-reserve SIZE` gives it.
- Adding a VM in the TUI and answering No now ends **Not done**, not
  "Done: VM is added".
- A task's window shows that it is still working between steps, and
  `vm add` says each step as it starts it, such as logging in to the VM.
- The add form warns when the VM is running and adding will shut it
  down; the plan `vm add` shows puts that warning first.
- The add form and `vm add`'s plan are shorter and plainer.
- **S** in the TUI edits the global settings (`global.toml`), each
  checked as it is typed; saving runs `rampire set global` and applies.
- A VM's name may have dots, as libvirt names often do
  (`rhel10.2-web`).
- `init` no longer counts the memory of VMs that are shut off and not
  managed, so a host with more VMs defined than it can run at once still
  gets a pool.
- The TUI's events panel and its notices of other people's changes work
  again: an event line that starts with `event=` was missed, and
  journalctl's "nothing matched" was taken for a failure.
- A yes-or-no question in the TUI marks the chosen button with arrows as
  well as its colour, leaves the other plain, and says what Enter
  answers, so No marked first can't be taken for Yes.
- `vm add` reads the VM's disk first and refuses, before changing
  anything, a VM whose system has no dnf or apt (Home Assistant OS, for
  one), where rampire-guest can't be installed. A VM shut down to put the
  guest package on its disk is started again if that fails, and an add
  that fails after changing the definition says how to retry or go back.
- The TUI's events panel shows a warning icon only when the journal
  can't be read.
- The host package shows in GNOME Software and other software centres
  with RAMpire's icon, summary, description and links, and installs from
  there; opening a newer `.rpm` upgrades it. **RAMpire** is in the
  desktop's applications too, and opens the TUI in a terminal after
  asking for your password (pkexec); the journal names you as the person.
- `tools/build.sh` with no arguments runs the gate and then builds the
  packages into `dist/`; `tools/build.sh check` runs the gate alone. A
  build that fails or is stopped leaves the earlier packages in `dist/`,
  and `install-check` works with Podman on SELinux hosts.
- The TUI's VM list asks an unmanaged VM's guest agent for RAMpire's
  files at most every 5 minutes (and again when the VM restarts, or on
  `r`), instead of every 30 seconds, so libvirt's log isn't filled with
  failed guest-agent calls.

## 0.5.0 (2026-11-15): Linux only

The first release: the controller for a Linux KVM host and the Linux
guest service, as packages for x86-64. Windows guests are not part of it
(their work is [parked](docs/roadmap.md#parked)), and packages for Arm
come later. Nothing is migrated from the earlier virtio-mem pool
controller: remove an old install and install again.

- `rampire vm add VM` puts a VM under RAMpire in one step: its settings
  suggested from its definition, the definition fixed (the old one kept),
  the guest service installed through the VM's QEMU guest agent, over SSH
  with RAMpire's own key, or, when neither gets in, onto the shut-down
  VM's disk with libvirt's disk tools. `rampire vm remove VM` takes it
  out again and puts the definition back to plain RAM, so Cockpit or
  `virsh` can change its memory again.
- `rampire vm upgrade VM` upgrades a VM's guest service to the build the
  host runs, the same ways `vm add` installs it. The TUI shows **update
  available** for a VM whose guest runs another build, `u` upgrades it,
  and `rampire doctor VM` reports it. Changing a VM's settings upgrades
  its guest service too.
- A TUI: `sudo rampire` on a terminal, or `ssh -t HOST sudo rampire`
  from any computer. The pool and each VM live; adding, removing and
  bringing back VMs; changing a VM's settings; the doctor, with fixes it
  can run; the journal by VM. Every action shows the command that does
  the same and is logged to the journal (`event=operator_change`).
  `rampire top` and `rampire watch` draw its screens alone.
  Actions run as background tasks, one at a time, their questions
  brought forward (`w` lists them). One key table makes the keys, the
  help you can type into, the status bar's hints and the command palette;
  chords such as `g g` show what can follow. Tab shows a VM's facts, a
  wide window previews the chosen VM, `/` filters the list and finds in
  the logs, `,` sorts, `m` changes the last columns, Space marks VMs for
  one action on several, and `y` copies through SSH with OSC 52. Other
  people's changes show as they happen. The TUI asks the terminal for its
  colours and background, draws each frame whole, only when something
  changes, and Ctrl+Z suspends it. Its layout and parts of its code come
  from Yazi (THIRD_PARTY_NOTICES.md). Midnight is the default theme. The
  histories are line charts on a GiB scale against time, coloured by
  load, with each VM's line, the host's free memory against its floor,
  and their figures; a VM's details chart plugged, requested and the
  guest's use.
- `rampire vm list` and `rampire doctor VM`.
- Each package installs `THIRD_PARTY_LICENSES` in its doc directory: the
  license and notices of every crate built into its binary
  ([THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)).
- Man pages, `man rampire`, `man rampire-COMMAND` and `man rampire.toml`,
  and `rampire-guest(8)` in the guest package, made from the same
  definitions as `--help`; shell completion for bash, zsh and fish. Options meant for scripts are under "For scripts" in
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
- Host and guest packages as RPM and `.deb` for x86-64, and static
  tarballs for other systemd distributions. The host package carries the
  guest package of the same build, which `vm add` installs. The guest
  package recommends `qemu-guest-agent`. Installs are checked on
  Debian 13, Rocky Linux 10 and Fedora 45.
- Every document names its schema and starts at version 1. The control
  channel port is `org.rampire.control.0`.
