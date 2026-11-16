# Testing

How RAMpire is tested, so a bug is found by a test before a person finds
it in the TUI. Every bug found gets a test that fails without its fix, at
the lowest of these layers that can show it.

| Layer | What it shows | Where | Runs |
|---|---|---|---|
| Unit tests | One function or screen: parsing, policy, a TUI view drawn into a buffer, a key pressed | Beside the code, `#[cfg(test)]` | In the gate, every change |
| Simulated host | Whole workflows through the flows and the TUI, against a fake libvirt, guest agent and journal that fail as real ones do | Being built (see below) | In the gate, every change |
| Live suite | What only real VMs show: the guest service installing and reporting in, the control channel, restarts, what is on the screen | `cargo xtask live` | On a host set up for it, before a release and after any change to a flow, the TUI's tasks, the guest or libvirt code |

## Unit tests

`cargo xtask gate local` runs them all. The TUI's tests draw each screen
into a buffer at several sizes and themes and press keys through
`App::on_key` ([development](development.md#the-tui-and-its-tests)).
A test that needs a task's flow sends its events (`Msg::Task`) as the
flow would.

## The live suite

```bash
cargo xtask live                       # install this build, every scenario
cargo xtask live --no-install add-running settings
cargo xtask live --keep                # leave the test VMs for a look
```

It installs this checkout's build on this host (`tools/build.sh
install`), makes small VMs named `rampire-test-*` from cloud images
(Debian 13 with a guest agent that runs commands, Rocky Linux 10 with one
that doesn't, as RHEL ships it), and works through what a person does:

| Scenario | What a person does | What must hold |
|---|---|---|
| `add-declined` | Starts an add in the TUI and answers No | The task says Not done; the definition is as it was; the VM isn't managed |
| `add-cancelled` | Starts an add and cancels it (`x`) as soon as it is working | However far it got, the VM is whole: managed and healthy, or not managed with its old definition back |
| `add-running` | Adds a running VM in the TUI: its menu, the form, the questions | The task's window opens as the form closes and stays open; each question is one expected, asked once; the add ends Done; the guest publishes; `doctor` finds nothing |
| `add-off` | Adds a VM that is shut off: the disk way | As above |
| `restart-open` | The VM restarts while the TUI is open | Nothing is written over the screen; the errors list has nothing from stderr; it reports in again |
| `settings` | `rampire set VM max_ram=3GB` | It exits 0 and the VM stays healthy |
| `upgrade` | `rampire vm upgrade VM` | As above |
| `vm-gone` | Deletes a managed test VM from libvirt behind RAMpire's back and restarts the controller | The controller runs and manages the others, logging `vm_setup_failed`; `vm remove` takes the gone VM out |
| `remove` | Removes each test VM in the TUI | Not managed after; its definition has none of RAMpire's parts |

After every scenario the controller must be running, and in
`restart-open`'s two minutes of the TUI polkitd may open no more than 40
descriptors: RAMpire asking polkit for every read once ran it out of
them, and it then refused the controller (`ls` reads polkitd's
descriptors).

Each command and the TUI run in tmux, at 150x45, and are read off the
screen as a person sees them. On every read the screen is checked for
what must never be on it: a row written over the TUI, `libvirt:`
messages, a panic. Questions are answered from a table: one that isn't
in it, or one asked twice, is a failure. The pool is never changed (No to
its question), so the host's own settings stay as they are.

The report, with the screen at each failure and a log of every question
and answer, is `target/live/report.md`. The test VMs are deleted at the
start and the end, unless `--keep`.

### Setting a host up for it

The suite runs as an ordinary user with these commands allowed through
sudo without a password, in `/etc/sudoers.d/rampire-live`:

```text
USER ALL=(root) NOPASSWD: /usr/bin/rampire, /usr/bin/virsh, /usr/bin/virt-install, /usr/bin/qemu-img, /usr/bin/journalctl, /usr/bin/dnf, /usr/bin/ls, /usr/bin/systemctl
```

(`dnf` installs the build; it is root in all but name.) The VMs' disks go
in a libvirt storage pool the user owns, named in `site/build.env` as
`RAMPIRE_LIVE_DIR=/path`:

```bash
sudo virsh pool-define POOL.xml   # type dir, target /path, owner and group the user's
sudo virsh pool-build rampire-test && sudo virsh pool-start rampire-test
```

It also needs `tmux`, `curl`, `qemu-img` and libvirt's `default` network,
and about 4 GiB of RAM and 3 GiB of disk for its two VMs.

## The simulated host

Being built: the flows and the TUI run in the test process against a
fake host, a step at a time, so a whole `vm add` (questions, waits, a VM
that restarts, a guest agent that stops answering, a command that fails)
is checked on every change without a VM. Until then, a workflow is
checked by the live suite.
