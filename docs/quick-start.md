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

## 3. Add the VM

Open the TUI on the host, or from your own computer over SSH:

```bash
sudo rampire                 # on the host
ssh -t HOST sudo rampire     # from another computer
```

Every VM on the host is listed. Choose the VM with the arrows, press
**Enter** and choose **Add it** (`a`):

1. Its settings come up, filled in from its definition: priority, minimum,
   maximum and free RAM, and how to log in to it. Change any, then
   **Enter**.
2. It shows everything adding will do, and asks once.
3. It does it, in the background: fixes the definition (keeping the old
   one), starts the VM, installs the guest service in it, restarts it if
   that is needed, applies, and waits for the guest to report in.

The same, as a command:

```bash
rampire vm add webserver
```

To install the guest service, RAMpire logs in to the VM: through its QEMU
guest agent when that runs commands, else over SSH as root at the address
libvirt has for it, with the host root's SSH key. Give another login with
`--guest-login USER@HOST`, or in the form. The guest service needs
systemd 253 or later: Debian 13, RHEL 10, current Fedora.

## 4. Watch it work

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
