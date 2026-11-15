# Roadmap

What is left to do, as found while using RAMpire on a real host. Each item
says what is wrong or missing, what is known, and what done looks like.
Work that is done goes in the [changelog](../CHANGELOG.md), not here.

## Desktop and software centres

- **Updates in GNOME Software.** Software installs the `.rpm` and
  upgrades from a newer one, with RAMpire's icon and description. It
  offers updates by itself only from a repository. Done looks like: the
  COPR repository published, with **Generate AppStream metadata** on, and
  the RPMs signed (see Parked).
- **A screenshot of the TUI.** The metainfo's first screenshot is now
  `docs/screenshots/tui.png`, drawn by the TUI's own renderer from a
  made-up host with four VMs (`tui/screenshot.rs`, regenerated as
  [development](development.md#the-tui-and-its-tests) says), at the raw
  GitHub URL once it is on `main`. The README asks for no simulated
  screenshot in release documentation. Needs an owner decision: keep
  this one, or replace it with a capture of the TUI on a real host.
- **The launcher on other desktops.** Tested on GNOME with Ptyxis.
  KDE (Konsole), Xfce and Debian's GNOME need checking: that `Terminal=true`
  opens a terminal, that `pkexec` keeps the terminal's colours (`TERM`),
  and that the polkit prompt shows.

## Testing

- **The simulated host.** Whole workflows (a `vm add` with its
  questions, waits and failures; a VM restarting with the TUI open) are
  checked only by the live suite, which needs a host set up for it and
  takes minutes. Done looks like: the flows and the TUI run in the gate
  against a fake libvirt, guest agent and journal that can fail as real
  ones do ([testing](testing.md#the-simulated-host)). The flows' system
  paths (`/etc/rampire`, `/var/lib/rampire`, `/run/rampire`) need to come
  from the context first, so a test can give them a folder of its own.

## Building

- **Install checks over a slow network.** `tools/build.sh install-check`
  downloads each distribution's dependencies (libvirt and libguestfs are
  large) every run, about ten minutes on this network, and a dropped
  download fails it. apt and dnf now retry. Done looks like: images with
  the dependencies installed, kept between runs like the build container.

## Parked

Not scheduled. Each needs an owner decision or an outside fix first.

- **W: Windows guests.** Moving to Hyper-V Dynamic Memory. Work on
  virtio-mem for Windows is paused until the `viomem` driver's removal
  defect is fixed ([administration](administration.md#windows-guests)).
- **Package repositories and signing.** 0.5.0 is published as files on
  GitHub Releases, with `SHA256SUMS`, by `tools/release.sh`
  ([releasing](releasing.md)). Signing the RPMs and `SHA256SUMS`, the apt
  repository and COPR are not decided.
- **Arm packages.** Releases are x86-64 only; `aarch64` RPMs and `arm64`
  `.deb`s need the build container to cross-compile and the install check
  to run on Arm.
- **Administrator deployments.** Rocky Linux 10, AlmaLinux 10 and
  Fedora 45, as a system administrator would deploy them, kept in place.
  Not started.
- **Home Assistant OS guests.** Its root is read only, so `vm add`
  refuses it. The guest service would need to run as a Home Assistant
  add-on (a container with the host's `/proc` and the control channel),
  published where Home Assistant finds add-ons.
- **Images in the TUI.** Real pictures through the Kitty graphics
  protocol and Sixel (for example the mascot in the header), where the
  terminal supports them, falling back to the icons.
