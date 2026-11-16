# Changelog

Every release of RAMpire, newest first. Versions follow semantic
versioning. Upgrade the controller first, then the guests: a controller
serves guests from its own minor release and the one before it.

A change someone using RAMpire would notice gets a line under
"Unreleased" in the commit that makes it. `tools/release.sh prepare`
turns that section into the release's entry, and the entry becomes the
release's notes on GitHub ([releasing](docs/releasing.md)).

## Unreleased

- After the host restarts, `apply` no longer fails every VM with
  "virtio-mem XML is missing current". libvirt shows a device's current
  size only once QEMU has reported it, which can take a while after a VM
  starts. Until then every `apply`, `vm add`, `vm remove` and `set` on the
  host failed, and a VM checked in that time later read as "its
  definition changed since RAMpire last checked this VM".
- A VM's memory is two sizes: Minimum RAM, the smallest it will be, and
  Maximum RAM, the biggest it may grow. `vm add` fits the VM to them:
  it starts with its minimum, and its memory device holds the rest up to
  its maximum. Before, the minimum had to match the RAM the VM already
  started with, and a maximum below that was taken without a word, after
  which the guest service refused it and stopped every few seconds.
- `rampire set` fits the VM again when a new minimum or maximum needs it,
  and asks whether to restart the VM for it. Until then the guest service
  keeps within the new sizes as nearly as it can, instead of stopping.
- The TUI's add and settings forms ask for Priority, Minimum RAM and
  Maximum RAM. Maximum RAM must be more than Minimum RAM. The add form no
  longer has Guest login, and upgrading a guest from the TUI no longer
  asks for an SSH login: RAMpire gets in through the guest agent, its own
  SSH key, or the VM's disk. `--guest-login` stays on the command.
- `free_ram` is no longer a setting, and nothing replaces it with a
  fixed size. A guest asks for more when its free memory falls below a
  16 MiB cushion, and how much it asks for is worked out from what it is
  doing: the shortfall, a small band, and what its demand will add before
  it can ask again, at the rate demand has been rising. It gives memory
  back once demand stops rising and its spare is more than its recent
  peak needs. `vm add --free-ram` is gone, `rampire set VM free_ram=...`
  is refused, and the TUI no longer shows it. A VM's file that still has
  a `free_ram` line loads as before, and the line is ignored.
- While `vm add` and `vm upgrade` wait for a guest, they say the first
  thing that isn't right, in the order the VM depends on it, and the
  guest service's own error in one line instead of its last log lines.
  A service that keeps failing is said to, not "activating".
- The controller no longer logs `guest_status_unpublished` for a status
  it sent on the control channel when the guest agent isn't answering,
  as when a VM is booting.
- The TUI no longer reports **Done** for a task that wasn't. Answering No
  to removing, restarting, upgrading, applying, recovering or setting up
  ends **Not done**, and so does a task with a part left: settings saved
  but not applied, a VM added or a guest upgraded whose guest service
  hasn't reported in yet. Each says what was and wasn't changed, and the
  **Done** notice says what was done.
- `vm remove` puts the VM's settings back when the controller can't be
  moved off it, so a failed removal leaves the VM managed as it was. A
  failure after that says the VM is no longer managed and what is left.
- A half-added VM's menu has **x**, which undoes the add
  (`rampire vm remove VM`, which puts its old definition back).
- `vm add` puts the VM back as it was by itself when a step fails before
  the VM is applied (the guest package wouldn't install, the VM didn't
  come back, the wait was cancelled): its old definition, from its next
  start, and none of the files it wrote. It says so, and the journal
  records `vm_add_undone`.
- VM names may have capital letters, as libvirt allows (`Web01`). In
  file names each capital is written `^` and the letter in lower case
  (`vms.d/^web01.toml`, the review `^web01`), so no two names share a
  file.
- `vm add` and `vm upgrade` install the guest service in a VM without dnf
  or apt: Fedora CoreOS and Silverblue (OSTree), and any other system
  with systemd 253 or later. It gets the static binary of the host's
  build in `/usr/local/libexec` and its own unit in
  `/etc/systemd/system`; the host package carries both in
  `/usr/share/rampire/guest/`. An OSTree VM takes it through a login
  only. Only a system with a read-only root (Home Assistant OS) is still
  refused before anything changes. The guest tarball installs the same
  way.
- A TUI task's window shows, once its question is answered, a moving
  mark, what it is doing now and for how long, where the buttons were, so
  a long step no longer looks finished; the status bar offers only the
  keys that do something then.
- `apply` starts the controller, and enables it at boot, when it isn't
  running and the configuration manages VMs, even when nothing else
  changed. A controller left stopped (removing and installing the
  package again stops it) used to resize nothing while every VM's guest
  looked as if it hadn't reported in. `doctor` checks that the
  controller runs, with `rampire apply` as the fix, and `vm add`'s wait
  says so first.
- Putting the guest service on a shut-off VM's disk waits up to 40 s for
  another reader of the disk to let go, such as the TUI reading what
  system the VM runs, instead of failing with "Failed to get write lock".
- `rampire doctor VM` for a VM RAMpire doesn't manage says when its
  definition still has RAMpire's parts from an earlier add (`vm remove`
  takes them out), and when its system is read only (Home Assistant OS),
  rather than suggesting an add that would be refused. What an add would
  change is listed as information, not as problems.
- A command's question stands apart from the text it is about (the
  difference to apply, the pool's reasoning) after a blank line.
- The TUI says when the controller stops, once, with the key that starts
  it again (**A**, apply); the header's "inactive" alone didn't say what
  to do.
- When `apply` or `vm add` can't re-check another managed VM's setup,
  the error names that VM and says to run its doctor; it read as if
  the VM being added were at fault.
- A managed VM the controller can't set up as it starts (libvirt no
  longer has it, or its definition lost RAMpire's parts) is left alone,
  logged as `vm_setup_failed`, and the other VMs are managed. It used to
  stop the controller, so no VM was resized.
- `rampire vm list` says what adding a VM will change as "adding it will
  ..." (it said "to fix:", as if a person had to), says when an earlier
  add left RAMpire's parts in a definition and that `vm remove` takes
  them out, and how to finish or undo an add that stopped part-way.
- A VM with a USB or SCSI device passed through (a TV tuner, a disk) is
  no longer asked whether its passed-through devices pin memory: QEMU
  copies their transfers, so they pin none. PCI and mediated (VFIO)
  devices are still asked about.
- Installing the guest package on a RHEL-family guest whose kernel has
  pressure stall information off no longer starts the guest service
  before the restart that turns it on: started, it asked for memory and
  the restart cut that request off, which latched the VM until a person
  recovered it. It starts after the restart. `vm upgrade` offers that
  restart.
- The TUI, `top`, `watch` and the commands share one libvirt connection
  as root instead of running `virsh` for every read. Each new connection
  is a polkit check: with the TUI open that was several a second, and
  polkit 125 leaks a file descriptor for each until it runs out, fails
  every check, and refuses the controller. With the TUI open there are
  now about 40 times fewer. They read the controller's journal every 5 s
  instead of twice a second.
- The controller tries again every 30 s when libvirt access is refused
  as it starts (exit 75), rather than stopping for good. polkit refuses
  while it restarts or is stuck; after a package upgrade restarted the
  controller at such a moment, it stayed stopped. Every other failure is
  still fail-stop.
- `vm upgrade`, and a settings change that upgrades the guest, say what
  they see every 20 s while they wait for the new build to report in,
  and why when it doesn't, as `vm add` does.
- The TUI no longer logs `tui_failed` when its terminal went away (a
  closed window, a dropped SSH connection) and so couldn't be restored.
- `doctor` no longer reports a guest that onlines hot-plugged memory as
  `online` with `online_policy=contig-zones` (Debian's default) as a
  problem: it gives memory back as RHEL's udev onlining does, and the
  docs say so. It says to switch to `auto-movable` should removals come
  back partial. `online_kernel` is still a problem.
- A VM's events in the TUI and `rampire watch` leave out the
  controller's "manages no VMs yet", from before the VM was added.
- The VM list's NOTE says **update** and **blocked** (was "update
  available (u)" and "fix first"), which fit when the VM's details are
  beside the list; they were cut short there.
- A VM's menu letter pressed in the list for a VM whose menu doesn't
  offer it, or with no VM chosen, says why instead of doing nothing.
- Saving the Add form (and the other settings forms) opens the task's
  window at once, instead of going back to the list until its first
  question. Adding a VM that is already being added doesn't start a
  second add (which asked everything twice); the one under way goes on.
- When the guest service runs but hasn't reported in, the wait says why
  from the controller's side: the reason it can't reach the VM's control
  channel, from its own log, rather than its error about the control
  files a guest on the channel never writes. The guest service's last
  log lines are shown too.
- Installing the guest service through the guest agent no longer fails
  with "Argument list too long": each part of a file sent is small
  enough for one argument.
- While `vm add` waits for the guest service to report in, it says every
  20 s what it sees: the VM not running, still on its old definition, the
  service in the guest not running (with its last log lines), or what the
  controller says. When the wait ends without it, the result says that
  cause instead of only naming `rampire doctor`. A VM still on its old
  definition after the add is offered a restart there and then.
- Nothing is written over the TUI any more: libvirt's own error printing
  is off (errors are read where they happen), and anything else written
  to stderr while the screen is open goes to the errors list (**e**).
- GNOME Software and other software centres show a screenshot of the
  TUI's dashboard first, then the diagram of how RAMpire moves memory.
- A VM's settings form shows the draft's values, which saving changes,
  not the applied copy's, and both settings forms say when the draft has
  changes that aren't applied yet. It no longer refuses to open until
  the VM's settings have been read.
- **A** in the TUI applies the draft (`rampire apply`), for a settings
  change saved but not applied.
- **u** upgrades each marked VM's guest service, not the one under the
  cursor alone; removing a VM from its live details goes back to the
  list.
- Commands exit **3** when they didn't do what they were for: a question
  was answered No, or a part is left (a VM added or a guest upgraded
  that hasn't reported in yet, a change saved that wasn't applied).
  They exited 0 before. `set --no-apply` still exits 0, and
  `apply --detailed-exitcode` exits 3 when the answer was No.
- `vm upgrade` asks for an SSH login when it can't get into the guest by
  itself, before offering to shut the VM down; No to shutting it down
  ends **Not done**, not failed.
- A task in the TUI can be cancelled while it waits for a VM to boot, a
  login or a guest to report in, not only at its next question; a
  cancelled login never shuts the VM down for the other way in. Quitting
  with tasks running stops them that way first, then closes.
- The TUI's list reads each addable VM's disk in the background, and a
  system rampire-guest can't be installed in (Home Assistant OS) shows
  as a problem before Add; **a** is offered once nothing stops adding.
- A managed VM's menu offers **b**, a restart onto its saved definition,
  when it runs an older one, and **c**, recovery, when its resizing has
  stopped. **D** shows the draft's difference, **B** rolls back.
- `rampire doctor --json` gives each runnable fix's command as `run`,
  which the TUI's doctor runs; **Enter** on a VM's summary in the
  doctor for every VM opens that VM's doctor. `rampire vm list --json`
  gains `add_restarts` and `runs_behind`.
- `init` says when it started a stopped controller, not "set up
  already".

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
