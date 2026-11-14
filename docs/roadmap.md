# Roadmap

Written 2026-11-14, when the previous roadmap closed. That roadmap built the
`rampire` command, the packages and the fleet features. This one makes the
product clean to use before it is shown working on real hosts and VMs.

The order matters. Each phase starts only when the one before it is done:

1. **S0** Clean pc101, so nothing from older versions is left on the test host.
2. **S1** Review the tooling and the deployment path as an end user sees
   them, and merge whatever has piled up twice.
3. **S2** One deployment as a hobbyist would do it, from the docs alone.
4. **S3** The remaining deployments, as a system administrator would do them.

## Rules for every task

- Every VM these tasks need is made with `cargo xtask testvm`
  ([tools](tools.md#test-vms)), never by hand ([AGENTS.md](../AGENTS.md)).
- **Every script used to deploy lives in the repository.** If a step needs a
  script, a scenario, a cloud-init file or an option that doesn't exist yet,
  it is written under `tools/`, `deploy/` or as an `xtask`/`rampire`
  option, committed, and documented before it is used. Nothing is run from
  a scratch directory. This is so each run can be repeated, and the scripts
  reviewed and refined afterwards.
- After each deployment, look back at the scripts it used: anything done
  twice gets combined, anything that was awkward gets fixed in the tool.
- Record each run's evidence path (`.artifacts/testvm/...`) against its task.

## S0: Clean pc101

Remove everything left on pc101 from `virt-mem-pool` and the older RAMpire
names, so later phases start from a known host. The removal is done with the
project's tools; if no tool covers a piece, add the tool first.

- [x] **S0.1** Inventory what is left: old binaries, systemd units, config
  and state directories, the `/etc/rampire/parked/` files and README,
  libvirt domains and disks from earlier test runs, and the old
  `virtio-mem-guest` service inside the parked guests (`debian13`,
  `safe_vm_1` to `safe_vm_4`, `minecraft101`). Write the list here.

  Done 2026-11-14 with the new `tools/linux/legacy.sh list`
  ([tools](tools.md#removing-pre-rampire-leftovers)). No old binary, unit,
  config, state directory, polkit rule or service account is left. What
  is left:

  | Item | Finding | Action |
  |---|---|---|
  | `/etc/rampire/parked/debian13.toml`, `safe_vm_1` to `_4.toml` | Their domains are gone | Remove |
  | `/etc/rampire/parked/README.md` | The hand procedure, replaced by `vm add` | Remove |
  | `/etc/rampire/parked/minecraft101.toml` | Domain exists, shut off, new port | Keep for `rampire vm add minecraft101` |
  | `~/scratch/old-install-20261114` (125 MB) | Backup of the old install | Remove |
  | `~/scratch/virtio-mem-gate` (6 GB), `virtio-mem-debian-wip` (3.4 GB), `virtio-mem-pool` | Old-name working folders | Remove |
  | `~/github/virtio-mem-pool` | This repository at `6c2d09d` under the old folder name, with `site/testvm.env` | Delete and clone again as `~/github/rampire` (owner) |
  | Other files in `~/scratch` (bundles, XML, `.rs` copies, `safe_vm/` 418 MB, `rampire-site/`, `dist/`) | Hand work from earlier sessions | Remove all (owner) |
  | Domains `homebridge101`, `dc1`, `TrueNas`, `win11_gpu`, `Tail` | Not RAMpire's | Leave |
  | `safe_vm` | Running, managed, new port | Keep |

- [x] **S0.2** Run the removal. Afterwards pc101 has only the current
  `rampire` build and the VMs the owner chose to keep.

  Done 2026-11-14 with `legacy.sh remove --apply --clear-scratch`. A
  second `list` shows only `minecraft101.toml` (kept) and the domains.
  `~/github/rampire` is a fresh clone at `71f1de7` with `site/testvm.env`
  carried across; `~/scratch` is empty; `rampire.service` is still
  active. The CI packages in `~/scratch/dist` went with it, so S1.3's
  download command is needed before S2.

## S1: Tooling and the end-user path

Before any deployment, read the product the way a new user meets it: the
README, [quick start](quick-start.md), [administration](administration.md),
`rampire --help`, the packages and the scripts. Look for anything that was
added on top of something else instead of replacing it.

- [x] **S1.1** Walk the [quick start](quick-start.md) on paper, command by
  command. Note every step that needs a second command, a manual edit, a
  file copied by hand or knowledge the page doesn't give.

  Walked 2026-11-14. Findings, each fixed the same day (owner: fix all
  six before S2):

  | # | Step | Finding | Fix |
  |---|---|---|---|
  | Q1 | 1. Install | The apt repository's setup is in [releasing](releasing.md#installing-from-the-repositories), a maintainer's page; the quick start only says "after adding the repository" | Done: the quick start has the apt lines |
  | Q2 | 4. Guest | Says `dnf install ./rampire-guest-VERSION...` with no word on getting the file into the VM, though both repositories carry `rampire-guest` | Done: `dnf/apt install rampire-guest` from the same repository |
  | Q3 | 2. Check | `doctor` is run expecting it to fail ("the only problem should be that nothing is applied yet") | Done: `init` runs `doctor`'s host checks first and stops on a problem, so the quick start no longer runs `doctor` first |
  | Q4 | 3. Set up | `rampire init` is always followed by `systemctl enable --now rampire`: two commands that always go together | Done: `init` enables and starts `rampire.service` once something is applied |
  | Q5 | 5. Add | `vm add` asks the guest's kernel release, so the user has to log into the VM for `uname -r` | Done another way: the publication only exists after the restart, so `vm add` reads `guest-get-osinfo` from the QEMU guest agent ("detected") and asks only when the agent doesn't answer. The guest packages now recommend `qemu-guest-agent` |
  | Q6 | 5. Add | A VM with no NUMA cell or `maxMemory` needs `virsh edit` by hand, then `vm add` again | Done: `--fix-xml` adds a NUMA cell (every vCPU, the boot RAM) and `<maxMemory>` in the same shown change. Too few slots in an existing `<maxMemory>` is still left to a person |

- [x] **S1.2** Inventory every entry point: `rampire` subcommands and
  options, `cargo xtask` commands, `tools/`, `deploy/`, `packaging/`. For
  each, who uses it (end user, administrator, developer) and what it
  overlaps with. Merge, rename or remove the overlaps.

  Inventoried 2026-11-14. The `rampire` commands have no overlaps. The
  overlaps are in the development tooling, most of it from before
  `rampire` existed:

  Owner: remove them, losing no function, and combine where it fits.
  Done 2026-11-14:

  | Entry point | Overlaps with | Done |
  |---|---|---|
  | `xtask doctor host` | `rampire doctor` (it only checked `virsh` is on `PATH`) | Removed |
  | `xtask controller-status` | `rampire status --json` | Removed. The one thing it added, the unit's state, is now in `rampire status` (a line first) and its JSON (`controller`, [json](json.md)) |
  | `xtask attestation` | `rampire vm attest` | Removed |
  | `xtask host-deploy` (950 lines) | The host package, plus `rampire check` and `apply` (which check attestations and keep the previous copy) | Removed. A development host installs packages from the new `cargo xtask package` ([development](development.md#installing-a-development-build)) |
  | `xtask linux package` (`debian.rs`) | `packaging/build.sh` (nfpm) | Removed; merged into `cargo xtask package`, which stamps both binaries and runs `build.sh`. CI runs the same command and takes its version from it, so a development package and a CI one are built one way |
  | `xtask watch`, `xtask pool` | `rampire watch`, `rampire top` (the same code) | Removed. `tools/windows/Watch-Pool.ps1` now runs the host's `rampire top`, with no checkout, as [names](names.md) already said it should |
  | `deploy/scripts/clone-vm.sh` | `xtask testvm` (image, seed, boot RAM, `device_mib`) | Removed |
  | `xtask host-run` | Nothing: the daemon in the foreground with a deadline and a host memory floor | Kept; its help now says `--config` is a directory |
  | `xtask preflight` | Windows-only, and Windows is parked | Kept with Windows. It reads `host-deploy`'s evidence, which is gone: when Windows comes back it reads `apply`'s evidence in `/var/lib/rampire/evidence/` instead |
  | `xtask linux deploy/status/uninstall` | The guest package | Kept: it installs a guest build with the workload over SSH, for live tests |
  | `tools/gate.sh` and `xtask gate` | Each other | Kept: `gate.sh` runs `xtask gate` on a Linux build host from Windows |
  | `xtask qualification`, `metrics` | Nothing | Kept: live-test evidence |
  | `tools/linux/legacy.sh` | Nothing | Remove after S3, once no host has old leftovers |

- [x] **S1.3** The overlaps already known:
  - `deploy/scripts/clone-vm.sh` and `xtask testvm` both make VMs from a
    cloud image and a cloud-init seed. Done: removed (S1.2).
  - `cargo xtask linux package` builds its own guest `.deb`, and
    `packaging/build.sh` builds another with nfpm. Done: one, `cargo xtask
    package` (S1.2).
  - Getting packages from a CI run to pc101 is done by hand
    (`gh run download`, then `scp`). Done another way: pc101 has a
    checkout and Cargo, so it builds the packages itself with `cargo xtask
    package`, the same way CI does. pc101 needs nfpm installed for that
    (owner, once); CI's own packages are still what a release ships.
- [ ] **S1.4** The docs: one place for each fact. Check that the README,
  quick start, administration and usage pages don't repeat each other or
  disagree, and that each page says who it is for.

  Found 2026-11-14: the settings are explained in four places (README,
  usage, administration, reference), and adding a VM in four (README,
  quick start, administration, reference). Proposed split:

  | Page | Reader | Holds |
  |---|---|---|
  | README | Anyone deciding | What it is, and a link to the quick start, not its commands |
  | Quick start | Hobbyist, first hour | Install to one managed VM |
  | Usage | Hobbyist, after that | Day-to-day changes in plain words, linking to reference for each setting |
  | Administration | Sysadmin | Layout, attestation, recovery, upgrades. No setting tables |
  | Fleet | Sysadmin, many hosts | Configuration management, metrics, JSON |
  | Reference | Both | The one definition of every command and setting |

  So far: the quick start, README flow, reference and administration are
  brought up to date with S1.1, and the README's link to the deleted
  `roadmap_next.md` is fixed. Still to do: take the setting tables out of
  usage and administration in favour of links to reference.

- [ ] **S1.5** Check the `testvm` scenarios cover the deployments in S2 and
  S3, and that their first-boot commands are the same commands the docs
  tell a user to type. A scenario that does something the docs don't say
  is a gap in the docs or in the tool.

  So far: the `clean-host-*` scenarios now run the quick start's
  `rampire init --yes` with no `doctor` beforehand and no `systemctl
  enable`, then check the unit is active and enabled. They still install
  from `--file`, not from the repositories, which don't exist before
  v0.1.0.

## S2: The hobbyist deployment (Debian 13)

One person, one host, following only the [quick start](quick-start.md). No
knowledge of the repository, no Ansible. The goal is the experience, not
just a working controller: anything that would stop or confuse a hobbyist
is a finding, fixed in the product or the docs.

- [ ] **S2.1** A clean Debian 13 host (`clean-host-debian13`) goes from
  nothing to a running controller, managing no VMs, with the host `.deb`
  and `rampire init`.
- [ ] **S2.2** A VM with no virtio-mem device goes from `vm add` to managed
  in one guided command. Make the guest with `linux-guest-debian13 --keep`,
  run `rampire vm add t-guest --fix-xml --review linux-standard --yes`,
  check `rampire status t-guest`, then remove it with
  `testvm down t-guest --apply`.
- [ ] **S2.3** Write up what the hobbyist path got wrong, fix it, and run
  S2.1 and S2.2 again until they go through with nothing done by hand.

## S3: Administrator deployments

The other supported families, as a system administrator would deploy them:
package manager, configuration management, repeatable runs. Each uses its
`clean-host-*` scenario.

- [ ] **S3.1** Rocky Linux 10 (`clean-host-rocky10`).
- [ ] **S3.2** AlmaLinux 10 (`clean-host-almalinux10`).
- [ ] **S3.3** Fedora 45 (`clean-host-fedora45`).
- [ ] **S3.4** Ubuntu 24.04 (`clean-host-ubuntu2404`). Ubuntu 24.04 ships
  libvirt 10.0, but `rampire doctor` requires 10.1, the first with
  `dynamicMemslots`. Expect `doctor` to fail there. The owner then decides
  whether to raise the supported Ubuntu release or to lower the minimum.
- [ ] **S3.5** The Ansible role (`ansible-almalinux10`): run three times on
  a clean host, it reports "changed", then no change, then "changed" after
  a settings change. Add a Rocky 10 scenario if the role should be shown
  there too.

## After this roadmap

The first release (signing key, GitHub Pages, COPR, tagging `v0.1.0`) is in
[releasing](releasing.md#one-time-setup) and is the owner's to set up.
