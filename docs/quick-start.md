# Quick start

For a first install on one host. From nothing to one VM whose memory
RAMpire manages, in about ten minutes. You need a KVM host with libvirt (libvirt 10.1 or later, QEMU 8.2
or later) and a Linux VM on it. Every command runs on the host as root
unless it says "in the VM".

## 1. Install

RAMpire 0.5 is for Linux on x86-64. Download the host package for your
distribution from the
[releases page](https://github.com/macg4dave/RAMpire/releases), with
`SHA256SUMS`, check it, and install it. For example, for 0.5.0:

```bash
version=0.5.0
base=https://github.com/macg4dave/RAMpire/releases/download/v$version
```

RHEL, AlmaLinux, Rocky Linux and Fedora:

```bash
curl -fL -O "$base/rampire-$version-1.x86_64.rpm" -O "$base/SHA256SUMS"
sha256sum -c --ignore-missing SHA256SUMS
dnf install "./rampire-$version-1.x86_64.rpm"
```

Debian:

```bash
curl -fL -O "$base/rampire_${version}_amd64.deb" -O "$base/SHA256SUMS"
sha256sum -c --ignore-missing SHA256SUMS
apt install "./rampire_${version}_amd64.deb"
```

The package manager brings in what the host package needs, and libvirt's
disk tools as a recommendation (`guestfs-tools`; on Debian
`libguestfs-tools`). You need only the host package: it carries the
guest package of the same build, and step 3 installs that in the VM. For a host without RPM or apt, the
release's static tarball, `rampire-VERSION-x86_64-linux.tar.gz`, has its
own `INSTALL.md`.

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
**Enter** and choose **Install RAMpire** (`a`):

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
libvirt has for it, with RAMpire's own key
(`/var/lib/rampire/ssh/id_ed25519.pub`), or as `--guest-login USER@HOST`
(Guest login in the form). When it can't log in, or the VM is off, it
uses libvirt's disk tools instead, which the host package recommends:
with the VM shut down, it puts the package on its disk, and the package
installs as the VM starts. The guest service needs systemd
253 or later: Debian 13, RHEL 10, current Fedora.

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
