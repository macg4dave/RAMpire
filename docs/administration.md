# Administration

For system administrators. How to install the RAMpire controller on a KVM host, add VMs to it,
install the guest services, recover from faults and upgrade. Development
builds and checks are in [development](development.md); the observation
tools are in [tools](tools.md).

## How it fits together

One controller (`rampire`) runs on the KVM host as the systemd unit
`rampire.service`. It manages every VM in its configuration
from one pool of virtio-mem device memory. In each VM a guest service
decides when it needs more or less memory and asks; the controller admits
the request within the pool, the VM's limits and the host's own reserve,
resizes the VM's virtio-mem device and reports the result.

Guest and host talk over a dedicated virtio-serial port,
`org.rampire.control.0` (the control channel). A VM without the port is
served through control files that the controller reads and writes with the
QEMU guest agent.

Run one controller per host: separate controllers cannot share a pool.

## Host layout

| Item | Location |
|---|---|
| Controller binary | `/usr/bin/rampire` |
| Unit | `/usr/lib/systemd/system/rampire.service` (from the tarball: `/etc/systemd/system/`) |
| libvirt access rule | `/usr/share/polkit-1/rules.d/50-rampire.rules` (from the tarball: `/etc/polkit-1/rules.d/`) |
| Service account | `rampire`, made by the package (`/usr/lib/sysusers.d/rampire.conf`) |
| Guest packages of this build | `/usr/share/rampire/guest/`, which `vm add` installs: the `.deb`, the RPM, and the static binary with its unit |
| Configuration (the draft) | `/etc/rampire/` (`global.toml`, `vms.d/<vm_name>.toml`, `reviews/`) |
| Recorded setups | `/etc/rampire/attestations/<vm_name>.attestation.json`, kept by `apply` |
| Applied copies | `/var/lib/rampire/applied/<id>/`, with `current` naming the one the controller runs |
| What each apply did | `/var/lib/rampire/evidence/<id>-apply.json` (and `-rollback.json`): the build, the copies switched between, every file's SHA-256, the VMs checked, whether the controller restarted |
| Durable request state | `/var/lib/rampire/state/<vm_name>-requests.json` (the unit's `StateDirectory`) |
| Guest publications for tools | `/run/rampire/<vm_name>.publication.json` |
| Deployment backups | `/var/lib/rampire/deployment-backups/` |

The unit runs `rampire daemon --config /var/lib/rampire/applied/current`.
The controller takes each VM's recorded setup from the `attestations/`
folder of the copy it runs and its state from the unit's `StateDirectory`.

## Installing the controller

`sudo rampire doctor` checks these once the controller is installed:
libvirt 10.1 or later and QEMU 8.2 or later, QEMU's `virtio-mem-pci`
with `dynamic-memslots` and `unplugged-inaccessible`, the service
account, the unit's socket group, the account's libvirt access (the
polkit rule), an applied configuration and the controller running. It prints the fix for each
problem.

Requirements: an x86-64 Linux KVM host with systemd, libvirt and QEMU,
`virsh` on the path, and polkit. Packages are built for the RHEL family
(RHEL, AlmaLinux, Rocky Linux), Fedora and Debian; installs are checked on
Rocky Linux 10, Fedora 45 and Debian 13, and RAMpire runs on RHEL 10.

1. Install the host package from the
   [release](https://github.com/macg4dave/RAMpire/releases)
   ([quick start](quick-start.md#1-install)): `dnf install
   ./rampire-VERSION-1.x86_64.rpm` or `apt install
   ./rampire_VERSION_amd64.deb`. It makes the `rampire` service account,
   installs the unit and the libvirt access rule, and leaves the
   controller off until it has a configuration. A host without RPM or apt
   installs the static tarball by its `INSTALL.md`; a development host
   installs a build of its checkout
   ([development](development.md#installing-a-development-build)).

2. Set it up:

   ```bash
   sudo rampire init
   ```

   `init` checks the host first, and changes nothing if something is
   missing. Then it writes a suggested `global.toml`: it reads the host's
   memory and every libvirt domain, sets aside the boot RAM of each VM
   with a virtio-mem device and all the memory of every other running VM
   (a shut-off one isn't counted: while it runs, growth stops at the
   reserve), and
   asks for `host_reserve`, the memory the host keeps, with the
   recommended eighth of the host (2 to 8 GiB) filled in to keep or
   change; `--host-reserve SIZE` gives it without asking. It never
   overwrites an existing `global.toml`. The TUI opens the same as
   **Set up this host** on a host without one (**I** opens it again);
   **S** there changes the global settings later. It applies it and leaves the
   controller enabled and running.

   To start from your own settings instead, write them in `/etc/rampire`
   from the examples in `/usr/share/doc/rampire/examples/` (see
   [Configuration](#configuration)), then:

   ```bash
   sudo rampire apply                    # check the draft and switch to it
   sudo systemctl enable --now rampire
   ```

3. Add each VM ([Adding a VM](#adding-a-vm)).

### Changing the configuration

`/etc/rampire` is a draft. The controller runs only a copy that
`rampire apply` has checked, so a mistake in the draft
never reaches it, through a restart or a reboot:

```bash
sudo nano /etc/rampire/vms.d/VM.toml
sudo rampire diff       # what would change
sudo rampire apply      # check, then switch
sudo rampire rollback   # back to the copy before
```

`rampire set` changes settings without an editor, keeping the file's
comments, then applies:

```bash
sudo rampire set webserver priority=150 max_ram=12GB
sudo rampire set global host_reserve=6GB
```

The target is a VM's name, or `global` for `global.toml`. A value is
written as a whole number, `true`/`false`, or text (`"12GB"`). Nothing is
written unless the changed file is valid and the whole configuration still
loads. `--no-apply` changes the draft only.

`apply` first records how each running VM is set up now (below), then
copies the draft and checks the copy like `rampire check` while the
controller keeps running, shows the difference and asks. Only then does
it keep the copy under `/var/lib/rampire/applied/`, point `current` at it
and restart the controller. A draft that fails changes nothing.
`--detailed-exitcode` exits 0 when nothing changed, 2 when something
changed, 1 on failure and 3 when the answer was No. `rollback` returns to the copy before the
current one, through the same checks. Each change is recorded under `/var/lib/rampire/evidence/`; the
`retention` setting decides how many copies and records are kept, never
removing the current copy or the one before it.

The daemon resolves `current` once when it starts, so an apply never
changes the files under a running controller.

[Usage](usage.md) walks through this for people new to the program.

## Configuration

One directory holds `global.toml`, one file per VM in `vms.d/`, review
profiles in `reviews/` and the recorded setups `apply` keeps in
`attestations/`. A missing
`global.toml` means every default. An empty `vms.d/` is valid: the
controller runs and logs that it manages no VMs. Only `vms.d/*.toml` files
are read, so editor backups and `.rpmnew` files there are ignored.
Comments (`#`) are kept when installed. Unknown or misspelt keys are
rejected. How sizes are written is in [reference](reference.md#sizes).

### How the settings act

Every key, its default and its limits are in
[reference](reference.md#globaltoml); this is how they work together.

A Linux removal that stops moving is settled after 380 s without
movement, not after `convergence_timeout_seconds`. The guest driver
retries a busy removal itself, 50 s, 150 s and 350 s after it first fails
and then every 300 s; the controller waits for the first three retries
and 30 s more, and that window also covers at least one retry after any
later movement. It is built in, from the driver's schedule.

Each VM occupies `max(requested, current)` bytes of the pool. Growth uses
only free pool capacity, highest priority first; among equal priorities the
request the guest issued first is served first, then VM name order. The
controller never takes memory from one VM to give to another. Admission
also keeps 256 MiB above `host_reserve` for changes between
planning and resizing.

How much a guest asks for is worked out from what it is doing; none of
it is a setting. It asks for more when its free memory falls below a
16 MiB cushion: the shortfall, a small band above it, and what its
demand will add before it can ask again, at the rate demand has been
rising over the last minute or so (twelve polls) and for two polls ahead.
A VM creeping up gets a small top-up; one climbing fast gets a large
grant at once; a single jump gets about that jump. When demand stops
rising the look-ahead falls to nothing, and memory goes back once the
spare exceeds the VM's recent peak demand and the band. Reclaim never
plans below half the cushion (also the level treated as critical), and
native Low pressure asks for at least 16 MiB. A Windows guest removes at
most a quarter of its maximum per request. A file written before this
may still have a `free_ram` line: it is ignored.

Guests take these settings from the host with every status. Changing a
VM's settings needs only a controller install, never a guest change.

## Adding a VM

`sudo rampire vm add VM` does it all in one command, in the order the VM
depends on each part.

**Checked first; nothing is changed until all of it passes and you agree:**

1. The host is set up and the controller runs: without it the guest has
   no one to report to.
2. The VM's definition can take RAMpire's parts (a NUMA cell, a free
   memory slot, the device and the control channel), and what it will
   look like with them.
3. The sizes: the minimum the VM will be and the maximum it may grow to,
   the maximum more than the minimum. The definition is then fitted to
   them: the VM starts with its minimum, and its device holds the rest
   up to its maximum.
4. The guest can take the guest service (a read-only system can't), and
   every other managed VM passes `apply`'s checks.
5. What it will do, including whether the VM is shut down, is shown, and
   you are asked once.

**Then, in this order:**

6. The fixed definition is saved. It takes effect only when the VM is
   next started from off, not at a reboot from inside it.
7. The guest service is installed: logged in to a running VM, or on the
   disk of one that is off (it installs as the VM starts). It is enabled
   and waits for its settings, so it does no harm before the controller
   knows the VM.
8. The VM is started, or shut down and started, once: the device, the
   control channel and the kernel's pressure information all come in
   with that start.
9. The review and settings are written and applied: the controller
   records the VM's setup and connects to its control channel.
10. The guest service gets its settings over the channel and reports in.
    While `vm add` waits it says every 20 s what it sees, the first thing
    in the list above that isn't right yet.

The steps below are what it writes, for doing them by hand or in a fleet.

1. **Device.** Give the domain a virtio-mem device with a stable alias and
   the options the controller requires. The device's `requested` size in
   the saved definition should be zero: the guest asks for what it needs.
   It needs a NUMA cell holding the boot RAM (`<numa>` in `<cpu>`) and a
   free slot in `<maxMemory>`; `vm add` adds both when they are missing.

   ```xml
   <maxMemory slots='1' unit='GiB'>16</maxMemory>
   ...
   <memory model='virtio-mem'>
     <target>
       <size unit='GiB'>8</size>
       <node>0</node>
       <block unit='KiB'>2048</block>
       <requested unit='KiB'>0</requested>
     </target>
     <alias name='ua-virtiomem0'/>
   </memory>
   ```

   QEMU must run the device with `dynamic-memslots` and
   `unplugged-inaccessible` on. RAMpire reads both from QEMU and
   refuses a VM without them.

2. **Control channel.**

   ```xml
   <channel type='unix'>
     <target type='virtio' name='org.rampire.control.0'/>
     <alias name='ua-rampire-control'/>
   </channel>
   ```

   libvirt chooses the host socket; the controller finds it in the domain
   XML. Without the channel the guest agent must run in the guest with file
   commands allowed (RHEL-family guests block them by default).

3. **Review.** Write a review profile, `CONFIG_DIR/reviews/NAME.toml`,
   that records what you checked, starting from
   [deploy/examples/reviews/linux-standard.toml](../deploy/examples/reviews/linux-standard.toml),
   and name it in the VM's file with `review = "NAME"`. Each answer says
   whether tooling detected it or a person confirmed it:

   ```toml
   version = 1
   guest_os = "linux"
   workload_reviewed = { value = true, source = "confirmed" }
   no_vdpa = { value = true, source = "confirmed" }
   # ... and the rest, as in the example
   guest_driver_version = { value = "linux 6.12.86+deb13-amd64", source = "confirmed" }
   ```

   `guest_os` must match the VM's settings. `guest_driver_version` is the
   `viomem` driver version on Windows and the kernel release on Linux.
   `workload_reviewed` can only be confirmed by a person.

4. **Settings.** Write `/etc/rampire/vms.d/<vm_name>.toml` and run
   `sudo rampire apply` with the VM running. The NUMA cell's memory is
   `min_ram`, and the device's size `max_ram` less that: `vm add` and
   `set` keep them so.

   `apply` records how the VM is set up: hashes of its live domain XML,
   QEMU's arguments and versions, and the device options, bound to the
   review. Every resize checks them again and refuses on any difference.
   The domain XML is read for every resize; what cannot change while QEMU
   runs is collected again when the domain restarts and at least every
   five minutes.

   When a VM changes (libvirt or QEMU upgraded, its definition edited),
   the next `apply` reads its definition again for every review answer
   taken from it, and records the new setup if they still hold. A change
   it can't read is asked; one that makes the VM unsafe to resize is
   said, and the VM stays refused. A saved definition takes effect only
   after a full shutdown and start: `sudo rampire vm restart VM` does that
   and applies.

5. **Guest service.** Install it in the guest (below). It waits, saying so
   in its log, until the controller has published its settings.

## Linux guests

The guest service is `rampire-guest`, run by systemd as an unprivileged
dynamic user ([unit](../guest-linux/systemd/rampire-guest.service)). It
has no configuration file.

| Item | Location |
|---|---|
| Binary | `/usr/libexec/rampire-guest` from the package; `/usr/local/libexec/rampire-guest` installed without one |
| Unit | `/usr/lib/systemd/system/rampire-guest.service` from the package; `/etc/systemd/system/rampire-guest.service` installed without one |
| Durable request state | `/var/lib/rampire-guest/control.json.request-state` |
| Control files, without the channel | `/var/lib/rampire-guest/control.json` and `control.json.host-status` |

`rampire vm add` installs the guest service of the host's build, the
way the guest's system takes it, from the host's
`/usr/share/rampire/guest/`:

| The guest's system | How it is installed |
|---|---|
| apt (Debian, Ubuntu) | The `.deb`, with apt |
| dnf (RHEL, Rocky, AlmaLinux, Fedora) | The RPM, with dnf |
| OSTree (Fedora CoreOS, Silverblue, Kinoite) | The static binary in `/usr/local/libexec` and its unit in `/etc/systemd/system`, since `/usr` is read only. Only through a login (the guest agent, or SSH with `--guest-login`): nothing may be written into an OSTree deployment from outside, so the disk way is refused |
| Any other system with systemd 253 or later (Arch Linux, openSUSE) | The static binary and its unit, the same way, through a login or on the disk |
| A read-only root (Home Assistant OS) | Not at all: there is nowhere to put the service. `vm add` and the TUI say so before anything is changed |

How it is installed is chosen again on each install or upgrade, by the
same rule, so a guest keeps its way. To install by hand, use the same
files from the host's `/usr/share/rampire/guest/`, or the release's
`rampire-guest` package or static tarball (its `INSTALL.md` says how,
and how to remove it). A static install isn't removed with a package
manager: `systemctl disable --now rampire-guest`, then remove the unit
and the binary.

The service needs pressure stall information to give memory back. RHEL's
kernels build it in but boot with it off (`CONFIG_PSI_DEFAULT_DISABLED`),
so on such a kernel the package adds `psi=1` to every installed kernel with
`grubby` (a static install leaves the kernel as it is: Fedora's kernels,
CoreOS's among them, have it on). It takes effect when the VM next starts, and until then the package
leaves the guest service stopped, so no memory request of its is cut off
by that restart: `rampire vm add` restarts the VM itself, `vm upgrade`
offers to, and otherwise `rampire vm restart VM` does it. An explicit
`psi=0` is left alone.

At start the service checks and logs its readiness: virtio-mem driver bound,
pressure stall information and its trigger, how hot-plugged memory is
onlined, the movable zone, swap and the commit model. The same report is
published with each sample's diagnostics.

Memory onlining: the kernel guarantees removal only from the movable zone.
Debian's default onlining (`online`, `contig-zones`) removed memory fully in
measured runs. RHEL leaves `auto_online_blocks` at `offline` and onlines
hot-plugged memory with a udev rule (`40-redhat-hotplug.rules`) as
`online`, which has the same effect; the readiness report names the rule.
If removals come back partial after long uptime, boot the guest with
`memory_hotplug.online_policy=auto-movable`. Avoid
`memhp_default_state=online_movable`: it confines the kernel's own memory to
boot RAM however large the guest grows.

Sizing under `auto-movable`: the kernel puts hot-plugged memory in the
movable zone only up to `auto_movable_ratio` percent (default 301) of the
boot RAM in kernel zones. Past that, new memory is split: about one
part in four (at the default ratio) goes to kernel zones, where removal is
not guaranteed, because kernel-zone memory of the same device raises its
movable allowance. On a live guest with 512 MiB boot, a 4 GiB spike put
640 MiB in `Normal`; that time it was all returned, because the pages
were freed user memory. At the default, a guest can hot-plug about three
times its boot RAM entirely movable, so keep `max_ram` within about four
times boot RAM where removal must be reliable: for example 8 GiB boot
supports `max_ram` up to about 32 GiB. The service publishes `auto_movable_ratio`, `auto_movable_numa_aware` and
`boot_ram_bytes` (from `/sys/firmware/memmap`), and when it receives its
settings it warns if `max_ram` is above that limit. With
`auto_movable_numa_aware=Y` (the default) the ratio also applies to each
NUMA node, so a guest with several nodes may reach its limit earlier.

Boot RAM can be very small, because the guest grows on demand once its
service runs. Measured on Debian 13 (kernel 6.12, 2026-11-13): the kernel
keeps back about 110 MiB of any boot size, the largest part a 64 MiB
`swiotlb` bounce buffer that x86 sets up because hot-pluggable memory
lies above 4 GiB. Untuned, 320 MiB booted cleanly (80 MiB free at idle)
and 256 MiB booted but starved while cloud-init ran. With `swiotlb=8192`
(16 MiB), `exim4` and `unattended-upgrades` disabled and cloud-init
disabled after provisioning, 256 MiB boots in about 10 seconds with
100 MiB free. Keep `swiotlb` rather than turning it off while the guest
has devices limited to 32-bit DMA (IDE, USB). Set `min_ram` to the boot
RAM so an idle guest can return its whole device, and save the device
with `<requested>` 0 so every boot starts on boot RAM alone.

Growth follows use, so a guest grows as a program touches memory, not
when it asks for it. Under the default heuristic overcommit
(`vm.overcommit_memory=0`) the kernel refuses a single allocation larger
than the guest's RAM and swap at that moment, before the guest can ask for
more: on a guest at 790 MiB, one 4 GiB `mmap` failed, while the same
4 GiB taken 512 MiB at a time grew the guest to 4.4 GiB in 21 seconds.
Programs that allocate as they go (ClamAV loading its signatures, for
example) are unaffected. For a workload that reserves a large block up
front, give the guest more boot RAM or a higher `min_ram`, or set
`vm.overcommit_memory=1` in the guest.

How Linux memory is read:

- Total and available are `MemTotal` and `MemAvailable`.
- File cache the guest is re-reading from disk (`workingset_refault_file`)
  counts as in use, so the guest grows until the re-reads stop and gives the
  memory back a step at a time once they have. Cache can therefore take pool
  memory up to `max_ram`.
- Pressure comes from `/proc/pressure/memory`. A trigger (100 ms of stall in
  1 s, or 200 ms in 2 s without `CAP_SYS_RESOURCE`) wakes the service and
  asks for memory at once. Reclaim waits until the ten-second average is
  zero.
- Under strict overcommit (`vm.overcommit_memory=2` with a RAM-scaled limit)
  the kernel's `CommitLimit` and `Committed_AS` are honoured. Otherwise the
  commit reserve never asks for memory.

The unit sets `OOMScoreAdjust=-900`: the service is what asks for more
memory, so it must be among the last processes the kernel kills.

## Windows guests

The Windows service is `RampireGuest`
(`C:\Program Files\RAMpire\rampire-guest.exe`, running as
LocalService). Its install configuration is
`C:\ProgramData\RAMpire\config.json`; limits and policy come from
the host as for Linux. It has no package yet (Windows is paused, below).
Installing it gives LocalService
access to the control channel's device instance
(`\\.\Global\org.rampire.control.0`) and checks that the service stays
running. Without the port the service uses the control files through the
guest agent.

**Windows on virtio-mem is paused.** The Windows `viomem` driver has a
removal defect, and all work on it is on pause until the driver bug is
repaired. The controller's two Windows-only workarounds (widening the
usable region and extending a stalled shrink) are archived in
[archive/viomem](../archive/viomem/) and no longer run: a Windows removal
that stops moving for the convergence timeout settles at what the guest
kept. Windows guests are moving to Hyper-V Dynamic Memory;
see [the roadmap](roadmap.md#parked), milestone W.

## The `rampire` command

Every operator command runs as root (`sudo rampire ...`); only
`rampire daemon`, which the unit runs, runs as the service account.
Commands that change or check the configuration (`check`, `diff`,
`apply`, `set`, `init`) work on the draft, `/etc/rampire`; the views and
recovery (`status`, `top`, `watch`, `recover`) on the applied copy the
controller runs, so a mistake in the draft doesn't hide what is running.
`--config` or `RAMPIRE_CONFIG` names another directory for both. `rampire --help` and
`rampire COMMAND --help` list every option.

The same command serves a person and a script:

- On a terminal, a command that changes something explains and asks.
  `--yes` answers yes. Without a terminal and without `--yes` it changes
  nothing and says so.
- `--json` writes a versioned document instead of text. It names its
  schema: `rampire.status-report` for `status` and `rampire.check` for
  `check`.
- `rampire --version` names the build: `0.5.0 (build 6b0e2db1a2c3)`.

| Exit code | Meaning |
|---|---|
| 0 | Done |
| 1 | The command failed, or `check` found a problem |
| 2 | The command line was wrong |

## Observing

```bash
sudo rampire top            # every VM and the pool
sudo rampire watch VM       # one VM
sudo rampire status [VM]    # the controller's status, as JSON
sudo rampire check          # settings, and each VM against its recorded setup
sudo rampire doctor         # what the host needs, with the fix for each problem
sudo rampire logs --follow  # the controller's journal
```

Each round the controller writes Prometheus metrics to
`/run/rampire/metrics.prom`: its build, rounds, the pool, and for each VM
its pool charge, `max_ram`, priority and whether it is stopped or
latched. There is no listening socket. To collect them with
node_exporter's textfile collector, add its user to the `rampire` group
and point `--collector.textfile.directory` at `/run/rampire`.

The controller logs every decision to the journal
(`sudo rampire logs`). See [tools](tools.md) for the
recorder's files and incident bundles.

## Recovering

**A latched controller.** When a resize ends in a state the controller
cannot prove safe, it latches (`actuation_latched` in the journal, `watch`
and `metrics`) and stops resizing until an operator clears it. Install the
current controller, then:

```bash
sudo rampire recover VM "why it is safe to resume"
```

It shows what it would do and asks. Then it stops the unit, settles a
stuck shrink at what the guest kept, records the reason in the VM's state
(as the service account, so the controller can still write it) and starts
the unit again. `--yes` answers for a script.

**A stopped controller.** The unit is fail-stop (`Restart=no`): a failure
needs a person to look. Read `systemctl status` and the journal, fix the
cause, then `rampire apply`, which starts it. Durable state means a pending
request is resumed, not lost. `rampire doctor` says when it isn't running.
One failure is tried again by itself, every 30 s: libvirt access refused at
start (exit 75), before anything was done, as when polkit is restarting.

**A guest service restart or guest reboot.** The guest's one unresolved
request is durable and is sent again unchanged until the host resolves it.
Never delete request state to get past an error: the request must still
receive its result.

**A VM that changed.** A resize refused with `... since RAMpire last
checked this VM` means the VM changed. `sudo rampire apply` checks it
again and records it, or says why it can't be resized.

**Startup memory.** A guest reboot keeps QEMU's live device size. Only a
full shutdown and start applies the saved definition (`requested` zero).
Do not clear request state to reduce RAM.

## Upgrading

Order: **controller first, then guest services.** A controller serves
guests from its own minor release and the one before it, so a 0.6
controller works with 0.5 guests while they are upgraded. Every document
the controller and the guests exchange or keep names its schema and
version; a reader refuses one it doesn't know, so it never guesses. The
[changelog](../CHANGELOG.md) says what each release changes.

1. Install the new host package over the old one, as in
   [Installing the controller](#installing-the-controller): `dnf install
   ./rampire-VERSION-1.x86_64.rpm` or `apt install
   ./rampire_VERSION_amd64.deb`. The package restarts a running
   controller onto the new binary; the configuration and the state under
   `/var/lib/rampire` stay.
2. `sudo rampire apply`, which records each VM's setup again where it
   changed, then check with `sudo rampire status`.
3. Upgrade each guest service. The TUI marks each VM whose guest runs
   another build with **update available**; `u` upgrades it, the same as

   ```bash
   sudo rampire vm upgrade VM
   ```

   which installs the guest package the new host package carries, logged
   in to the VM or, agreed to, on its disk with it shut down for a moment.
   Changing a VM's settings upgrades its guest too. `sudo rampire doctor
   VM` reports a guest of another build, with this fix.
