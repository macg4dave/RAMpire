# Roadmap

For the owner and contributors: what is being built next, in what order,
and what stands in the way. Written 2026-11-14. The previous roadmap (clean
pc101, review the tooling, the hobbyist deployment) is finished and is in
Git history (`git show ab447c2:docs/roadmap.md`); the one before it is at
`git show 71f1de7~1:docs/roadmap.md`.

Three milestones, in order. Each starts when the one before it is done.
T and M were done on 2026-11-15, except the checks noted under T; U is
next:

1. **T** A TUI on the KVM host, used locally or over SSH: the pool, one
   VM, adding and removing VMs, and diagnosing them.
2. **U** A real user installs RAMpire on their own host from the package
   files, guided by us, using the TUI.
3. **M** Man pages, and the split between commands for people and the
   interface for scripts.

Work that is not scheduled is under [Parked](#parked).

## Rules for every task

- Everything is done with the product: the packages and the `rampire`
  commands ([AGENTS.md](../AGENTS.md)). There is no test tooling beside
  it. A step the product can't do is a finding, fixed by adding it to the
  product.
- Deployments stay in place and are improved as findings are fixed; they
  are not torn down after a run.
- After each milestone, look back at what it took: anything done twice
  gets combined, anything that was awkward gets fixed in the product.

## T: The TUI

One full-screen program on the KVM host. A person reaches it on the host's
console or with `ssh -t host sudo rampire` from any terminal. It needs
nothing on the person's own computer but an SSH client.

`rampire` with no command opens the TUI when stdin and stdout are
terminals. Piped, or with `--help`, it prints the help as now. Like every
other command, it runs as root; read-only screens without root would need
their own way of reading, and aren't part of T.

| Screen | What it shows and does | Built on |
|---|---|---|
| Dashboard | Every managed VM and the pool: each VM's charge, the free pool, host memory against the floor, resizes under way, pool history, events | `rampire top` |
| VM details | One VM: boot RAM, device size, requested and plugged memory, a resize under way and its progress, the guest's own figures, the last grant and what held it back, its events. Change its settings, restart it onto its saved definition, open its doctor | `rampire watch`, `rampire set`, `rampire vm restart` |
| Add a VM | The host's VMs that RAMpire doesn't manage; for one, its settings, then one question | `rampire vm add` |
| Remove a VM | Stop managing a VM and delete what RAMpire kept for it | `rampire vm remove` |
| Doctor | The host's checks and each VM's, problems first, each with its fix | `rampire doctor` |
| Logs | The controller's journal, filtered by VM | `rampire logs` |

### Where it starts from

- `top` and `watch` are live views drawn by our own renderer
  (`crates/rampire-ops/src/dashboard.rs`, ANSI escapes). They take no
  input except Ctrl+C.
- `vm add`, `vm remove` and `apply` ask their questions by reading stdin,
  in the `rampire` binary (`rampire/src/vm_lifecycle.rs` and `confirm` in
  `rampire/src/main.rs`). A TUI can't reuse them as they are.
- `doctor` checks only the host. What is wrong with one VM is spread over
  `status`, `check`, `watch` and the journal.
- No command lists the host's VMs that aren't managed, which the add
  screen needs.

The TUI is built with [Ratatui](https://ratatui.rs) and `crossterm`,
which give input, resizing, layout and a test backend for T6.1. `top` and
`watch` move onto them too, so there is one renderer and the dashboard's
own ANSI code goes. Both crates are checked against the rule for
importing third-party code (`git show 71f1de7~1:docs/roadmap.md`).

### T1: Groundwork in the commands

Done first, so the commands and the TUI share one owner for each job.

- [x] **T1.1** One owner for the guided flows. Move the steps of `vm
  add`, `vm remove` and `apply` (show a change, ask yes or no, ask one of
  several answers, report progress) into `rampire-ops` behind an
  interface for asking questions. The terminal answers it as today; the
  TUI answers it in a dialog. Done when the commands' questions and output
  are unchanged (tests show it) and nothing in the flows reads stdin.
- [x] **T1.2** `rampire vm list`: every VM on the libvirt connection,
  marked managed or not managed. For one that isn't managed, what
  `vm add` would change and whether its guest service has
  published. For a managed one, its state as `status` gives it. With
  `--json` ([json](json.md)).
- [x] **T1.3** `rampire doctor VM`: everything that can be checked about
  one VM, each problem with its fix, as the host checks do:
  - its definition: the device, the control channel, the balloon, free
    `maxMemory` slots (what `vm add` inspects);
  - the running VM against its recorded setup (`check`);
  - a latch, and the `recover` command that clears it;
  - the guest's publication: present, and how old it is;
  - the guest's readiness report: driver bound, how hot-plugged memory
    is onlined, the movable zone, `max_ram` against the auto-movable
    limit ([administration](administration.md#linux-guests));
  - the last refused or partial request, and what held it back.

  `rampire doctor` with no VM keeps the host checks and adds one line per
  managed VM.
- [x] **T1.4** Split `top` and `watch` into what they read and how they
  draw it, so the TUI draws the same figures from the same reading.

### T2: The screens

- [x] **T2.1** The frame: a header with the host, the controller's state
  and the build; keys or a tab bar between screens; `?` lists the keys;
  `q` quits; a status line. Keys are plain letters, arrows, Enter, Esc
  and Tab, so they work in Windows Terminal, PuTTY, macOS Terminal, tmux
  and screen. Nothing needs a function key, Ctrl with an arrow, or a
  mouse.
- [x] **T2.2** Dashboard (the table above). Enter on a VM opens its
  details.
- [x] **T2.3** VM details, with its actions: change `priority`,
  `min_ram`, `max_ram` and `free_ram` (`set`, then apply), restart,
  remove, doctor.
- [x] **T2.4** Add a VM, from `vm list`'s unmanaged VMs, through T1.1:
  the definition change, the one review question, the files it will
  write, the restart, the apply, then the guest's first publication as it
  arrives. A VM whose guest service hasn't published says so, with the
  install command for the VM's system
  ([quick start](quick-start.md#3-add-the-vm)).
- [x] **T2.5** Remove a VM: stop managing it and delete what RAMpire
  kept for it, saying what it removed.
- [x] **T2.6** Doctor (T1.3), host and VMs. Where a fix is a `rampire`
  command, offer to run it.
- [x] **T2.7** Logs, scrolling, filtered by VM.
- [x] **T2.8** Every action shows the command line it ran, for example
  `rampire vm add webserver`, so a person learns the commands
  and can script what they did.

### T3: Over SSH, at any size

- [x] **T3.1** Sizes: usable from 80×24 up. Smaller than that, it says
  the size it needs. Narrow screens drop columns, as `top` does below
  110. It redraws when the terminal is resized.
- [x] **T3.2** Colour: 256 colours where the terminal has them, 16
  otherwise, none with `NO_COLOR` or `TERM=dumb`. Nothing is shown by
  colour alone: steady, growing and shrinking also have words or
  symbols. ASCII lines and bars when the locale isn't UTF-8 (the
  dashboard's `Glyphs` already do this).
- [ ] **T3.3** Slow links: it sends only what changed. Keys still answer
  at once over a link with 200 ms of latency.
  Not measured: rendering sends only changed cells (Ratatui), and every
  reading and action runs off the screen's thread, so keys don't wait on
  libvirt or the journal. Still to do: a run over a link with 200 ms
  added.
- [x] **T3.4** Leaving: Ctrl+C, `q`, a dropped SSH connection (SIGHUP)
  and a crash all give the terminal back as it was. An action under way
  either finishes or stops where a rerun finishes it (`vm add` already
  resumes).
- [x] **T3.5** Run without root, it says how to start it (`sudo
  rampire`) instead of failing on the first file it reads.

### T4: Logging and errors

- [x] **T4.1** Every change made from the TUI is logged to the journal as
  the command logs it, with the person (`SUDO_USER`) and that it came from
  the TUI.
- [x] **T4.2** An error is shown in full beside the action that caused
  it, with its fix, and kept in an errors list. It doesn't disappear on
  the next refresh.
- [x] **T4.3** If the TUI itself fails, the terminal is restored and the
  error printed, with where the journal entry is.

### T5: Looking great

Done once T1 to T4 work: the screens work first, then they are polished.
The TUI should look like a finished product: one colour theme, aligned
panels, gauges and sparklines from Ratatui, and icons where they help a
person read the screen at a glance. Nothing depends on what is installed
on the person's own computer, such as a special font.

- [x] **T5.1** Unicode icons, the default: symbols such as ⚙ (settings),
  ● (running), ✓ (ok), ⚠ (problem), ▣ (device) and ◆ (VM), plus block
  and braille characters for bars and history. One table maps each
  meaning to its Unicode and ASCII icon, so a screen names "ok", never
  "✓".
- [x] **T5.2** Plain ASCII, the fallback: `[ok]`, `[!!]`, `#` and `.`
  bars, used when the locale isn't UTF-8, with `TERM=dumb`, or when asked
  for (T3.2).
- [x] **T5.3** Themes: the current navy palette as the default, a light
  one, and one that uses only the terminal's own 16 colours, so it
  matches a person's terminal theme.
- [ ] **T5.4** Unicode, ASCII and each theme are drawn in the T6.1
  tests, and checked by eye on pc101 over SSH from Windows Terminal and
  PuTTY.
  The tests draw every screen in every palette, at 256, 16 and no
  colours, in Unicode and ASCII. Checked on pc101 over SSH from a tmux
  terminal at 80x24 in Unicode, 16-colour ASCII (`TERM=xterm LANG=C`)
  and `TERM=dumb`. Still to do: by eye from Windows Terminal and PuTTY.

### T6: Checking it

- [x] **T6.1** Each screen is drawn in tests at 80×24, 120×40 and
  200×60, in the gate.
- [x] **T6.2** On pc101, over SSH from Windows Terminal and from PuTTY,
  with the kept deployments (`deb13_rampire`, `rhel10_rampire`): add a
  new VM, change its settings, run its doctor, remove it and bring it
  back, all from the TUI. Fix the findings and run it again until
  nothing needs a second terminal.
- [x] **T6.3** Docs: the quick start and [usage](usage.md) open the TUI
  where they now run `top`; [reference](reference.md) lists the TUI and
  its keys.

**Done when** a person with only SSH to the host can add a VM (quick
start steps 4 and 5), change its settings, diagnose it and remove it from
the TUI, at 80×24.

Done 2026-11-15 on pc101 with `deb13_rampire`, over SSH at 80x24:
`set deb13_rampire priority=110` from the settings form, park and bring
back, forget and add again (its review question answered for the
throwaway VM), the doctor and the logs, all from the TUI. Findings, each
fixed in the product:

- `vm add` and `vm list` failed on a domain with `<cputune>`
  before `<cpu>`: the text edit took one for the other.
- A guest publishes only once a host sends its settings, so before `vm
  add` none has; `vm list` now says whether the guest service is
  installed.
- Under sudo's own pty a dropped SSH connection sends no SIGHUP, and the
  TUI spun on the dead terminal ignoring SIGTERM. It now ends on a
  hang-up, and a second signal ends it at once.
- A choice's letter `k` was taken for "up"; a long diff pushed the
  question out of the dialog; long lines were cut at 80 columns.

Open from T: T3.3 and T5.4 above. The `deb13_rampire` guest onlines
hot-plugged memory with Debian's `contig-zones` policy, which `rampire
doctor deb13_rampire` reports with its fix (a kernel parameter in the
guest); it is left for the owner to decide.

## U: A real user installs it, guided

Someone who isn't the owner installs RAMpire on their own KVM host and
puts one of their own Linux VMs under it. They type every command; we
guide and take notes. The packages are the files of a GitHub release,
since no repository is published yet ([Parked](#parked)).

RAMpire runs entirely on the host and talks to the guest over a virtio
channel. It needs no SSH keys and no network setup between host and
guest. The only SSH in this milestone is the user's own, into their host,
to use the TUI.

### U1: Before the session

- [ ] **U1.1** Their systems meet the requirements: a host with
  libvirt 10.1 and QEMU 8.2 or later, a guest with systemd 253 or later
  (Debian 12 and RHEL 9 guests don't qualify).
- [x] **U1.2** The [quick start](quick-start.md) gets a route from
  package files: where the files come from, `dnf install
  ./rampire-VERSION-1.x86_64.rpm` or `apt install ./rampire_VERSION_amd64.deb`,
  and how the guest package gets into the VM. Done 2026-11-15: the files
  of the GitHub release, checked against its `SHA256SUMS`; the host
  package carries the guest package.
- [ ] **U1.3** `vm remove` takes out of the definition what `vm add`
  put in: the virtio-mem device, the control channel, and the
  `maxMemory` and NUMA cell if it added them, leaving the VM on its boot
  RAM with libvirt's defaults. It takes effect at the VM's next cold
  start. Anything else in the definition is the user's to manage.
- [ ] **U1.4** One build for the session: a release's packages and
  their checksums (0.5.0 or later). The user keeps
  it for the whole session and upgrades with the same route if a fix
  comes in.

### U2: The session

- [ ] **U2.1** Install: the host package and `rampire init`; we watch
  for anything they have to ask about.
- [ ] **U2.2** Guest service in their VM, then add it from the TUI. They
  answer the review question about their own programs: we never answer
  it for a real VM.
- [ ] **U2.3** Use: the dashboard and the VM's details while it works;
  change one setting; the doctor.
- [ ] **U2.4** Trouble: they work it out from the doctor and the docs;
  we step in only when they are stuck. Note what they tried.

### U3: Afterwards

- [ ] **U3.1** A findings table: every question they asked, every place
  they stopped, every command they had to be told. Each finding is fixed
  in the product or the docs.
- [ ] **U3.2** Their deployment stays in place, if they want it, and is
  upgraded as fixes come.

**Done when** the user went from nothing to a managed VM with no command
outside the product and the quick start, and the findings are fixed.

## M: Man pages and the script interface

Commands for people are short and act by default; scripts get explicit
flags, `--json` and stable exit codes. This milestone writes that split
down and ships it. It comes after U, but M1 and M4 are small and move
before U if T finishes early, so the real user has man pages.

- [x] **M1** Man pages generated from the same command definitions as
  `--help` (`clap_mangen`), when the packages are built: `rampire(8)`, a
  page per subcommand (`rampire-vm-add(8)` and so on),
  `rampire-guest(8)` and `rampire-watch-ram(1)`. Both packages install
  theirs.
- [x] **M2** `rampire.toml(5)`: `global.toml`, `vms.d/VM.toml` and the
  review profiles, generated from the settings' definitions in
  `rampire-core`. The gate checks [reference](reference.md)'s settings
  tables against them, so there is still one definition of each setting.
- [x] **M3** The script interface: list every option and command that is
  for scripts (`--json`, `--yes`, `--detailed-exitcode`, `--no-apply`,
  `--interval-seconds`, `vm add`'s explicit flags, and the rest), put them
  under a "For scripts" heading in `--help` and the man pages, and write
  one page that says what stays stable from the first release: the JSON
  documents and their versions ([json](json.md)), exit codes, journal
  event names ([journal](journal.md)).
- [x] **M4** Shell completion for bash, zsh and fish (`clap_complete`),
  installed by the host package.
- [x] **M5** Code comments that cite old roadmap task IDs (`O3.5`,
  `O4.2`, `P1` and others) name the docs page or the behaviour instead,
  since those roadmaps are gone.

**Done when** `man rampire` and `man rampire.toml` work on a host from
the packages, and a script writer can tell from them which interfaces
won't change.

Done 2026-11-15: both work on pc101 from the packages, which install
cleanly on Debian 13, Rocky 10 and Fedora 45 (`tools/build.sh
install-check`). `rampire(8)` has the "For scripts" options and what
stays stable; docs/fleet.md's script interface says the same.

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
  terminal supports them, falling back to the icons (T5). After the
  TUI works and looks right without them.
