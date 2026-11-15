# Roadmap

What is left to do, as found while using RAMpire on a real host. Each item
says what is wrong or missing, what is known, and what done looks like.
Work that is done goes in the [changelog](../CHANGELOG.md), not here.

## Adding VMs

- **VMs whose system has no dnf or apt.** `vm add` installs rampire-guest
  as an RPM or a `.deb`, through a login or on the VM's disk. Home
  Assistant OS has neither, and its root filesystem is read-only, so
  `vm add` now refuses it before changing anything (it reads the disk with
  `virt-inspector`). Image-based systems such as Fedora CoreOS and
  Silverblue (rpm-ostree) are in the same place. Done looks like: a way to
  run the guest service on them (a static binary in a writable folder with
  its own unit, or a container or add-on where the system has one), chosen
  by what the disk inspection finds, and documented per system.
- **Say so in the list, before Add.** The VM list and its facts don't yet
  say that a VM can't take the guest package; you find out when you press
  Add. The disk inspection takes about ten seconds per VM, so it needs to
  run once per VM in the background and be kept, like the guest probe.
- **Undo a half-finished add by itself.** When `vm add` fails after it has
  changed the definition (the guest package wouldn't install, the VM
  didn't come back), it now says so and names `rampire vm remove VM` to put
  the old definition back, and a VM it shut down for the install is
  started again. Done looks like: the add puts the old definition back
  itself, or the TUI offers it in one key.
- **Which answer the add question starts on.** Enter answers what is
  marked, and the marked button now shows plainly; No is marked first, as
  for every question. Once a person has reviewed the settings form and the
  plan, Yes may be the better start for this one question. Needs an owner
  decision.
- **Names libvirt allows and RAMpire doesn't.** VM names may now have
  dots, but not capital letters (`Web01`), which libvirt allows. The name
  is a file name in `vms.d`, so this needs a mapping that can't collide.

## Memory

- **The pool after adding VMs.** `init` sets the pool once, from what the
  VMs hold then. Adding a VM frees all but its boot RAM, but the pool
  stays as it was until someone runs `rampire set global pool=SIZE` (or
  **S** in the TUI). Done looks like: `vm add` and `vm remove` offer the
  new pool, with the same reasoning `init` shows.

## Desktop and software centres

- **Updates in GNOME Software.** Software installs the `.rpm` and
  upgrades from a newer one, with RAMpire's icon and description. It
  offers updates by itself only from a repository. Done looks like: the
  COPR repository published, with **Generate AppStream metadata** on, and
  the RPMs signed (see Parked).
- **A screenshot of the TUI.** The metainfo shows the "how it works"
  picture. Software centres expect a screenshot of the program: a picture
  of the TUI, published at a fixed URL.
- **The launcher on other desktops.** Tested on GNOME with Ptyxis.
  KDE (Konsole), Xfce and Debian's GNOME need checking: that `Terminal=true`
  opens a terminal, that `pkexec` keeps the terminal's colours (`TERM`),
  and that the polkit prompt shows.

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
- **Images in the TUI.** Real pictures through the Kitty graphics
  protocol and Sixel (for example the mascot in the header), where the
  terminal supports them, falling back to the icons.
