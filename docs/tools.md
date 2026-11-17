# Tools

The live views, all from the host package: the TUI (`rampire` with no
command), `rampire watch` and `rampire top`. `top` and `watch` are the TUI's dashboard and VM details
alone ([reference](reference.md#the-tui)). The host's views read the
controller's journal, libvirt and each guest's publication. `CONFIG` below
is the configuration directory the controller runs (installed as
`/etc/rampire`); with several VMs, `--vm NAME` picks one. They follow the
controller unit, `rampire.service`.

## Guest publications

While the controller runs it keeps each guest's latest validated
publication in `/run/rampire/<vm_name>.publication.json`, readable
by root and the service account, as the tools run as root. The tools read that copy first. Without it (the
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
  - ADDED: what the device adds to the boot RAM (`+0.12G`), not the VM's
    size
  - whether the device is steady, growing or shrinking, and by how much
  - `min_ram`–`max_ram` (on terminals at least 110 columns wide)
  - how much of the RAM the guest has now is in use, and, where the
    limits column doesn't fit, the most it may grow to (`42% of 1.00,
    max 4.00`)
  - the guest's pending request

  A publication older than 30 s is marked stale.
- **Pool history**: a line chart of the pool's use over the last five
  minutes, in GiB against time, green, amber then red as the pool fills,
  with each VM's own line in its colour and the pool's size. Beneath it,
  the host's free memory as bars coloured against its floor, and the
  figures: now, the average, the peak, the least free, the host's low and
  the change over what is shown. A short window shows coloured columns.
  A VM's details chart what is plugged, what was requested, and the
  guest's use against its total, with the count of grows and shrinks.
- **Pool events**: grants, refusals, completed requests, latches and
  control-channel changes from the controller's journal, for every VM.

Guest figures come from the controller's publication copies, so the view
sends no guest-agent traffic. They are empty while the controller is
stopped. `--interval-seconds N` sets the refresh period (default 1),
`--events N` the number of events (default 8) and `--once` prints one
plain snapshot. Ctrl+Z goes back to the shell until `fg`. `q`, Esc or
Ctrl+C leaves the last view on the screen as plain text.

From any computer with an SSH client, the whole TUI:

```bash
ssh -t HOST sudo rampire
```

On a Windows workstation, the same from PowerShell or Windows Terminal
with Windows' own OpenSSH client; `ssh -t HOST sudo rampire top` shows
the pool alone.

## Building

```bash
tools/build.sh            # the gate, then the packages into dist/, in the build container
tools/build.sh check      # the gate alone
tools/build.sh package    # the packages alone, into dist/
tools/build.sh install    # build them and install the host package here
```

`tools/build.sh` runs `cargo xtask gate` and `cargo xtask package` in the
build container; see [development](development.md).
