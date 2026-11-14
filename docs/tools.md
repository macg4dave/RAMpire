# Tools

The live views: `rampire watch` and `rampire top` from the host package,
and `rampire-watch-ram` from the guest package. The host's views read the
controller's journal, libvirt and each guest's publication. `CONFIG` below
is the configuration directory the controller runs (installed as
`/etc/rampire`); with several VMs, `--vm NAME` picks one. They follow the
controller unit, `rampire.service`.

## Guest publications

While the controller runs it keeps each guest's latest validated
publication in `/run/rampire/<vm_name>.publication.json`, readable
by any local user. The tools read that copy first. Without it (the
controller stopped) they read the guest's control file through the QEMU
guest agent, which works only for a guest on the control files: a guest on
the control channel writes no file, so only the controller's copy shows it.

## Watching the controller

```bash
rampire watch [VM]
```

A live view of one VM: device size, requested and current memory, the
guest's latest publication and the controller's events.

## Watching the pool

```bash
rampire top
```

A live view of every VM in the configuration on one screen, read-only like
`watch`:

- **Pool**: one bar split into each VM's charge (`max(requested,
  plugged)`, as the controller counts it) in that VM's colour, then the
  free part. Below it are the pool size, the charge, the free pool, the
  resizes under way and host `MemAvailable` against the controller's
  floor. Peak charge and lowest free and host memory are kept for the
  session.
- **VMs**: one row per VM, in host-list order, showing:
  - priority
  - plugged size
  - whether the device is steady, growing or shrinking, and by how much
  - `min_ram`–`max_ram` (on terminals at least 110 columns wide)
  - how much of the guest's RAM is in use
  - the guest's pending request

  A publication older than 30 s is marked stale.
- **Pool history**: the pool's charge over time, stacked by VM, against
  the pool size.
- **Pool events**: grants, refusals, completed requests, latches and
  control-channel changes from the controller's journal, for every VM.

Guest figures come from the controller's publication copies, so the view
sends no guest-agent traffic. They are empty while the controller is
stopped. `--interval-seconds N` sets the refresh period (default 1),
`--events N` the number of events (default 8) and `--once` prints one
plain snapshot. Ctrl+C leaves the last view on the screen as plain text.

From a Windows workstation, open it over SSH:

```powershell
powershell -ExecutionPolicy Bypass -File tools\windows\Watch-Pool.ps1 [-NewWindow]
```

`Watch-Pool.ps1` connects with `ssh -t` and runs the host's installed
`rampire top` through `sudo`, so the host needs no checkout. The host is a
site value: pass `-HostName`, set `RAMPIRE_TOP_HOST`, or copy
[deploy/examples/pool.env.example](../deploy/examples/pool.env.example) to
`site/pool.env`. `-NewWindow` opens a separate 140-column window, and
`-IntervalSeconds` and `-Events` are passed through.

## Watching guest RAM

```powershell
powershell -ExecutionPolicy Bypass -File tools\windows\Watch-Ram.ps1
```

`Watch-Ram.ps1` runs inside a Windows guest and reads memory directly with
`GetPerformanceInfo`. It needs no configuration and no running service.

```bash
rampire-watch-ram [-i SECONDS] [--csv]
```

`rampire-watch-ram` comes with the Linux guest package and runs in the
guest without root. It reads `/proc/meminfo` on every tick: a memory
table, usage bars, memory pressure and page-cache refaults, and a history
of total and in-use RAM. Piped, it prints plain snapshots; `--csv` gives
one row per sample.

## Building

```bash
tools/build.sh            # the gate, in the build container
tools/build.sh package    # the packages, into dist/
tools/build.sh install    # build them and install the host package here
```

`tools/build.sh` runs `cargo xtask gate` and `cargo xtask package` in the
build container; see [development](development.md).
