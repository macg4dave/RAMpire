# Reference

For looking things up: every `rampire` command and every setting, defined
once here. `rampire --help` and
`rampire COMMAND --help` show the same, from the same definition.

## Options every command takes

| Option | Meaning |
|---|---|
| `--config DIR` | The configuration directory. Commands that change or check it use the draft, `/etc/rampire`; the views and recovery use the applied copy the controller runs. Also `RAMPIRE_CONFIG` |
| `--connect URI` | The libvirt connection; default the configuration's `libvirt_uri` |
| `--yes` | Answer yes to every question |
| `--json` | Write a versioned JSON document ([JSON documents](json.md)) |

Every command but `daemon` runs as root. Exit codes: 0 done, 1 failed or
a problem found, 2 a wrong command line. The options for scripts, and
what stays the same between releases, are in
[the script interface](fleet.md#the-script-interface). `man rampire`,
`man rampire-COMMAND` (`man rampire-vm-add`) and `man rampire.toml` are
installed with the host package, and shell completion for bash, zsh and
fish.

## The TUI

`sudo rampire` with no command, on a terminal, opens the TUI: on the
host's console, or from any computer with `ssh -t HOST sudo rampire`.
Piped, or with `--help`, it prints the help instead. It needs a terminal
of at least 80x24.

Everything starts from one screen: the pool, every VM on the host
(managed, half-added and not managed), the pool's history and its
latest events. On a window at least 140 columns wide and tall enough, the
chosen VM's details are beside the list, live; for a VM RAMpire doesn't
manage, its facts. **Enter** on a VM opens its menu beside its row, which
offers only what fits that VM:

| The VM is | Its menu |
|---|---|
| Managed | `v` live details (as `rampire watch` shows them); `s` settings: Priority, Minimum RAM, Maximum RAM and Free RAM (`priority`, `min_ram`, `max_ram`, `free_ram` in its file); `o` its doctor; `l` its logs; `c` restart it, so its saved definition takes effect; `k` remove it: stop managing it and delete what RAMpire kept for it |
| Not managed | What would stop adding it; `a` add it: its settings (Priority, Minimum, Maximum and Free RAM, filled in from its definition, and Guest login), then everything adding will do, asked once; `o` its doctor |
| Half-added | `a` finish adding it; `o` its doctor |

A menu's letters also work from the list and from a VM's details,
without opening the menu (in the list, `k` moves; removing is in the
menu). In the menu, Shift with a letter copies the item's command line
instead of doing it.

### Tasks

What an action does runs as a task, in the background, so the screen
stays free. Tasks run one at a time, in the order they were started,
since each changes the host's configuration; the others wait their turn.
A task's question comes forward when nothing else is open, with the
task's output above it: `y` or `n`, or the arrows and Enter (No is
chosen first); Esc leaves it waiting. The status bar shows how many of
the tasks started are left, red once one has failed, and how many are
asking.

`w` lists the tasks. Enter shows one's output and its question; `x`
cancels one: a waiting one at once, a running one at its next question,
which is answered with a stop, so nothing more is changed; `D` clears the
finished ones. `q` asks first while tasks run: they stop with the TUI,
and a `vm add` that stopped finishes when it is run again.

What a task did slides in briefly at the bottom right; an error stays
longer, and in the errors list (`e`). A change someone else makes while
the TUI is open, with a command or another TUI, slides in too, with who
made it, and the screen reads again (from the journal's
`event=operator_change` lines).

### Keys

Every key is in one table, which the help, the status bar and the
command palette are made from. `?`, or F1 anywhere, lists the keys for
what is open; type a few letters to find one (a key itself first, then a
word of what it does), and Enter does it. A key followed by "then" is the
first of two: a popup shows what can follow.

Anywhere:

| Key | Does |
|---|---|
| Ctrl+P or `:` | The command palette: type a few letters of anything the TUI can do, for the chosen VM or anywhere, each with its key; a theme shows as the cursor reaches it |
| arrows, `j` `k`; `g` then `g`, `G`, Home, End; Ctrl+U, Ctrl+D; PgUp, PgDn, Ctrl+B, Ctrl+F | Move in any list: a row, to either end, half a page, a page |
| `w` | The tasks |
| `e` | The errors list |
| `g` then `d`, `l`, `w`, `e` | Go to the doctor, the logs, the tasks, the errors |
| Esc | Back to the list, or close what is open; in the list, clear a filter and the marks |
| `r` | Read again |
| `t` | The next colour theme: midnight (the default: the mascot's colours on near-black), midnight in 256 colours, light, or the terminal's own 16 colours, which match your terminal's theme |
| `i` | Unicode or ASCII icons |
| `?`, F1 | The keys |
| `q`, Ctrl+C | Quit; while tasks run it asks first, and Ctrl+C twice quits at once |
| Ctrl+Z | Back to the shell; `fg` brings the TUI back |

The VMs:

| Key | Does |
|---|---|
| Enter | The chosen VM's menu |
| Tab | Its facts: what `vm list`, the pool and its settings say, and the commands that show more; Left and Right go through the VMs, `y` copies the chosen fact |
| `/` | Show only the VMs whose name has what is typed, as it is typed; Tab completes a name, Enter keeps the filter, Esc puts back what was there |
| Space, Ctrl+A | Mark the chosen VM, or every VM shown (again: none). A menu's letter then does its item for each marked VM, after one question that lists the commands; `s` opens one settings form for them all, whose empty fields leave each VM its own |
| `,` then `o`, `n`, `r`, `p`, `u` | Sort as the pool lists them, by name, by RAM plugged, by priority, by the guest's memory in use; a capital letter reverses. VMs without the figure go last |
| `m` then `g`, `l`, `d`, `t` | The last columns: the guest's memory and request, the RAM limits, the device, what `vm list` says |
| `y` | Copy the chosen VM's name |
| `d`, `l` | The doctor for the host and every VM; the logs |

A VM's details: Left, Right, Up and Down go to the VM before or after;
Tab, its facts; `y` copies `rampire watch VM`; the menu's letters work.
The doctor: Enter runs the chosen fix when it is a `rampire` command,
after a question; `y` copies it; `/` filters the findings; `p` shows only
the problems. The logs: `/` finds a text and marks its lines, `n` and `N`
go to the next and the one before; `f` shows one VM's lines; `y` copies
the chosen line. The settings form has a labelled box for each setting,
checked as you type: a value that won't do is marked in red with the
reason (a size needs its unit, such as `8GB`; Minimum RAM can't be more
than Maximum RAM, nor Free RAM as much). Under the boxes, a word about
the chosen setting and the command Save runs. Tab, Shift+Tab and the
arrows go between the settings, Enter saves and applies, Esc cancels.

Text fields take the usual keys: Home, End, Ctrl+A, Ctrl+E, Ctrl+U,
Ctrl+K, Ctrl+W, Alt+B, Alt+F. Up and Down recall what was typed for the
same question earlier this session, and Tab takes the suggestion shown
dim after the cursor.

`y` and Shift with a menu's letter copy with OSC 52: the text goes to the
clipboard of the computer you sit at, through SSH and sudo, in terminals
that allow it (most do; in tmux, `set -g set-clipboard on`).

### How it looks

Every action runs the same steps as its command, asks the same
questions, and shows the command line that does the same. Each change is
logged to the journal as `event=operator_change` with the person and
`via=tui` ([journal](journal.md)).

As it opens, the TUI asks the terminal whether it shows 24-bit colour and
whether its background is light. Midnight is in true colour where the
terminal keeps it, whatever `TERM` says, and in 256 colours where it
gives one of 256 back; on a light background, light comes next after midnight
in `t`'s order, and the terminal theme's dim text is darker. A terminal
that doesn't answer is taken at its `TERM`: true colour on any
`256color` terminal, else 16 colours, or none with `NO_COLOR` or
`TERM=dumb`, when nothing is asked. Icons are ASCII when the locale isn't
UTF-8. Each frame is drawn whole, as one synchronized update where the
terminal has them, so a slow link never shows half of one; the screen is
drawn only when something changes.

If the TUI fails, the terminal is given back and the error printed, and
logged as `event=operator_change action=tui_failed`.

## Commands

### Setting up and checking

| Command | What it does |
|---|---|
| `rampire init` | Checks the host as `doctor` does and stops on a problem; then suggests `global.toml` from the host's memory and its VMs (never over an existing one), creates the configuration folders, applies, and enables and starts the controller |
| `rampire doctor [VM]` | Checks libvirt 10.1+, QEMU 8.2+ with virtio-mem's options, the service account, the socket group, libvirt access and an applied configuration, with the fix for each problem, and gives one line per managed VM. With a VM, everything about it: its saved and running definition (device, control channel, balloon, memory slots), its recorded setup, a latch, the guest's publication and readiness report (driver, onlining, pressure signal, `max_ram` against the movable zone), and the last request and what held it back |
| `rampire check` | Checks the draft and every VM against its recorded setup; changes nothing |

### Changing the configuration

| Command | What it does |
|---|---|
| `rampire diff` | Shows the draft against the applied copy |
| `rampire apply [--detailed-exitcode]` | Copies the draft, checks the copy while the controller runs, shows the difference, asks, then makes it current and reloads or restarts the controller. A failing draft changes nothing |
| `rampire rollback` | Returns to the applied copy before the current one, through the same checks |
| `rampire set TARGET KEY=VALUE... [--no-apply]` | Changes settings in a VM's file, or in `global.toml` with `global`, keeping comments, then applies |

### VMs

| Command | What it does |
|---|---|
| `rampire vm add VM [--guest-login USER@HOST] [--priority N] [--min-ram SIZE] [--max-ram SIZE] [--free-ram SIZE] [--review PROFILE] [--kernel RELEASE] [--no-guest-install]` | Starts managing a VM. It shows everything it will do and asks once; then it fixes the definition, keeping the old one (a NUMA cell, `maxMemory`, the device, the control channel and the guest agent channel, and the memory balloon turned off), starts the VM, installs or upgrades `rampire-guest` in it (through its guest agent, or over SSH as `--guest-login`, default root at the address libvirt has for it), restarts it if that is needed, writes its review and settings, applies, and waits for the guest to report in. The sizes default to ones suggested from the definition. It stops first if another managed VM would fail `apply`'s checks |
| `rampire vm restart VM` | Shuts a VM down and starts it again, so its saved definition takes effect, then applies |
| `rampire vm list` | Every VM on the libvirt connection: managed, unfinished (a `vm add` that didn't apply) or not managed. For one that isn't managed, what `vm add` would change, what would still stop it, and whether its guest service is installed |
| `rampire vm remove VM` | Stops managing a VM at once, and removes its settings, recorded setup, request state, definition backups and the review profile `vm add` wrote for it. It keeps its memory until its next cold start, which begins on boot RAM alone |

### Watching

| Command | What it does |
|---|---|
| `rampire status [VM]` | The controller's view of one VM or all of them |
| `rampire top [--once] [--interval-seconds N] [--events N]` | The TUI's dashboard alone, live; `q` leaves |
| `rampire watch [VM] [--once] [--interval-seconds N] [--guest-interval-seconds N] [--events N] [--no-guest]` | One VM's details alone, live |
| `rampire logs [-f] [--since TIME] [--vm VM]` | The controller's journal and the changes people made; `--vm` only the lines about one VM |

### Recovering

| Command | What it does |
|---|---|
| `rampire recover VM REASON` | Clears a VM's latch once a person has checked it is safe: stops the controller, settles a stuck removal at what the guest kept, records the reason, starts the controller |
| `rampire daemon` | Runs the controller; the unit runs this |

## Files

| Path | What it holds |
|---|---|
| `/etc/rampire/global.toml` | Settings for the whole host |
| `/etc/rampire/vms.d/VM.toml` | One VM; the file's name is the VM's |
| `/etc/rampire/reviews/PROFILE.toml` | A review profile |
| `/etc/rampire/attestations/VM.attestation.json` | How a VM was set up when `apply` last checked it |
| `/var/lib/rampire/applied/ID/`, `current` | Applied copies, and the one the controller runs |
| `/var/lib/rampire/evidence/` | What each apply and rollback did |
| `/var/lib/rampire/state/` | Each VM's durable request state |
| `/var/lib/rampire/domain-backups/` | Definitions `vm add` replaced |
| `/usr/share/rampire/guest/` | The guest packages of the host's build, which `vm add` installs |
| `/var/lib/rampire/ssh/id_ed25519` | RAMpire's SSH key for logging in to guests, made the first time `vm add` needs it |
| `/run/rampire/metrics.prom` | Metrics, every round |

## Sizes

Sizes are written in quotes with a unit: `"512MB"`, `"3GB"`, `"1.5GB"`.
Units are binary whatever the spelling: MB and MiB mean the same, and
1 GB = 1024 MB. Spaces and lower case are allowed (`"3 gb"`). A number
without quotes is a count of bytes, so `max_ram = 8` means 8 bytes and is
refused by the alignment rules below.

In a VM's file, `min_ram` and `max_ram` are multiples of 1 MiB, and
`free_ram` is a multiple of 8 MiB and less than `max_ram`. On the running
VM the limits are also aligned to its device's block, and `free_ram` must
be eight blocks or more: with the 2 MiB block `vm add` gives a new
device, use multiples of 16 MiB. Whole gigabytes always fit. Applying a
size that doesn't fit says which one.

## `global.toml`

Every setting is optional.

| Setting | Default | Meaning |
|---|---|---|
| `pool` | none | Device memory shared by every VM, on top of each one's boot RAM. Required with more than one VM |
| `host_reserve` | `"4GB"` | Memory the host keeps for itself; growth stops before crossing it |
| `observe_only` | `false` | Decide and log everything, but change no VM's memory |
| `poll_interval_seconds` | `5` | Seconds between host polls and guest samples, 1 to 10. A publication older than six polls is stale |
| `command_timeout_seconds` | `10` | Limit on each libvirt command |
| `convergence_timeout_seconds` | `300` | How long a Windows removal may stop moving before it is settled at what the guest returned, and the deadline for each settle command. Linux removals use a built-in 380 s ([administration](administration.md#how-the-settings-act)) |
| `libvirt_uri` | `"qemu:///system"` | The libvirt connection |
| `retention` | `10` | Applied copies, and records of what each apply did, kept under `/var/lib/rampire`; at least 2, so there is always a copy to roll back to |

## `vms.d/VM.toml`

One file per VM, named for the libvirt domain with `.toml` added:
lower-case ASCII letters, digits, `-` and `_`, at most 64 characters. The
name isn't repeated inside. Only `vms.d/*.toml` files are read, so editor
backups and `.rpmnew` files there are ignored.

| Setting | Meaning |
|---|---|
| `alias` | The VM's virtio-mem device alias |
| `priority` | Who is served first when memory is short; higher first |
| `min_ram` | The least RAM the VM's system sees, boot RAM included; memory is never taken back below it. The guest's kernel keeps some boot RAM for itself, so a `min_ram` equal to boot RAM grows the VM a little at once |
| `max_ram` | The largest it may grow. For Linux, at most about four times boot RAM |
| `free_ram` | Spare memory the guest keeps; it asks for more below this and returns memory above it ([sizes](#sizes)) |
| `guest_os` | `"linux"`, or omitted for Windows |
| `review` | Its review profile in `reviews/` |
| `telemetry_path` | Optional host file for a diagnostic copy of each publication |
| `install_path`, `service_account` | Windows only: where the guest service is installed and the account it runs as |

`global` is not a VM name: `rampire set` uses it for `global.toml`.

## `reviews/PROFILE.toml`

What RAMpire can't prove from the running VM. Each answer is
`{ value = ..., source = "detected" | "confirmed" }`; `workload_reviewed`
can only be confirmed by a person. See the
[example profile](../deploy/examples/reviews/linux-standard.toml).

| Setting | Meaning |
|---|---|
| `version` | 1 |
| `guest_os` | `"linux"` or `"windows"`, matching the VM |
| `workload_reviewed` | The VM's programs cope with memory being added and taken away |
| `no_vdpa`, `no_rdma_migration`, `no_vfio_nvme`, `no_mlock`, `no_secure_virtualization`, `no_unsupported_vhost_user` | None of these, each of which can hold on to guest memory, is used. `no_rdma_migration` is detected on a host with no RDMA hardware |
| `balloon_resize_inactive` | The memory balloon isn't used to resize the VM; detected when there is none (`vm add` turns it off) |
| `memory_slot_budget`, `vfio_mapping_budget` | The budgets reviewed for the device |
| `guest_driver_version` | The kernel release on Linux (`"linux 6.12.0"`), the `viomem` driver version on Windows |
