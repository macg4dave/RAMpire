# Quick start

For a first install on one host. From nothing to one VM whose memory
RAMpire manages, in about ten minutes. You need a KVM host with libvirt (libvirt 10.1 or later, QEMU 8.2
or later) and a Linux VM on it. Every command runs on the host as root
unless it says "in the VM".

## 1. Install

RHEL, AlmaLinux, Rocky Linux and Fedora:

```bash
dnf copr enable macg4dave/rampire
dnf install rampire
```

Debian:

```bash
curl -fsSL https://macg4dave.github.io/RAMpire/apt/rampire.asc \
  | install -D -m 0644 /dev/stdin /etc/apt/keyrings/rampire.asc
curl -fsSL -o /etc/apt/sources.list.d/rampire.sources \
  https://macg4dave.github.io/RAMpire/apt/rampire.sources
apt update && apt install rampire
```

On RHEL, AlmaLinux and Rocky Linux, `dnf copr` comes from
`dnf-plugins-core`. The packages are also on the
[releases page](https://github.com/macg4dave/rampire/releases)
(`dnf install ./rampire-VERSION.x86_64.rpm`,
`apt install ./rampire_VERSION_amd64.deb`). For a host without a package,
the static tarball has its own `INSTALL.md`.

## 2. Set up

```bash
rampire init
```

`init` first checks the host: libvirt and QEMU versions, the service
account and libvirt access. If something is missing it says what and how
to fix it, and changes nothing. Otherwise it reads the host's memory and
its VMs, writes `/etc/rampire/global.toml` with a suggested pool and host
reserve, applies it, and starts the controller, which also starts at boot.
It now runs, managing no VMs yet.

## 3. Install the guest service, in the VM

Add the same repository in the VM as on the host in step 1, then:

```bash
dnf install rampire-guest      # or: apt install rampire-guest
```

It needs systemd 253 or later: Debian 13, RHEL 10, current
Fedora. It also installs the QEMU guest agent, so the host can read the
VM's kernel release in the next step.

## 4. Add the VM

Open the TUI on the host, or from your own computer over SSH:

```bash
sudo rampire                 # on the host
ssh -t HOST sudo rampire     # from another computer
```

Every VM on the host is listed. Choose the VM with the arrows, press
**Enter** and choose **Add it** (`a`). It runs in the background, and
each of its questions comes up for you to answer with `y` or `n`. It runs
the same steps as the command, which you can also type yourself:

```bash
rampire vm add webserver --fix-xml
```

`vm add` reads the VM's definition and:

- adds what it lacks, after showing you the exact change: a NUMA cell and
  `maxMemory` if it has none, a virtio-mem device and the control
  channel. It turns off the memory balloon libvirt adds by default,
  which would work against RAMpire. The old definition is kept;
- reads what it can from the VM and the host, and asks you one
  question: whether the VM's programs are fine with memory being added
  and taken back while they run;
- writes `/etc/rampire/vms.d/webserver.toml`, with limits from the VM's
  boot RAM;
- offers to start or restart the VM so its new definition takes effect,
  then applies. Applying records how the VM is set up, and every resize
  checks that it still is.

## 5. Watch it work

The TUI shows the VM and the pool live. **Enter** on the VM opens its
menu: its details (`v`), settings (`s`) and doctor (`o`). Without the TUI:

```bash
rampire status
rampire top
```

The VM starts small and grows as its programs need memory. When it goes
quiet it hands memory back for other VMs. `rampire doctor` checks the
whole setup at any time, and `rampire doctor webserver` everything about
the one VM.

## Next

- [Usage](usage.md): changing settings, checking it works, what to do when
  something goes wrong.
- [Reference](reference.md): every command and setting.
- [Running a fleet](fleet.md): configuration management, metrics and
  scripting.
