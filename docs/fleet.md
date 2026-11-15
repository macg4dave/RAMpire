# Running a fleet

For administrators of many hosts. RAMpire's command works the same for a
script as for a person. This page
collects what automation needs.

## The script interface

Commands for people are short and act by default, asking before each
change. A script uses the same commands with explicit options. These are
listed under **For scripts** in `rampire --help`, each command's
`--help` and the man pages:

| Option | Commands | Does |
|---|---|---|
| `--yes` | every command | Answers yes to every question a script may answer. Never a review question about a VM's programs: give `vm add` a reviewed profile with `--review` instead |
| `--json` | `status`, `check`, `doctor`, `vm list` | A versioned JSON document instead of text |
| `--detailed-exitcode` | `apply` | 0 unchanged, 2 changed, 1 failed, 3 not applied (No was answered) |
| `--no-apply` | `set` | Change the draft only, to apply later |
| `--review`, `--kernel`, `--priority`, `--min-ram`, `--max-ram`, `--free-ram` | `vm add` | Every answer and setting given, so nothing is asked |
| `--once`, `--interval-seconds`, `--events`, `--guest-interval-seconds`, `--no-guest` | `top`, `watch` | One plain snapshot, and how much is read |

- Every command runs as root.
- With `--yes`, or without a terminal, a command never asks. Without a
  terminal and without `--yes`, a command that would change something
  changes nothing and says so.
- `rampire --version` names the build.

### What stays the same

From the first release, these don't change without the
[changelog](../CHANGELOG.md) saying so:

- the JSON documents, each naming its `schema` and `version`; a document
  only gains fields within a version ([JSON documents](json.md));
- exit codes: 0 done, 1 failed or a problem found, 2 a wrong command
  line, 3 not done: a question was answered No, or a part is left (a
  VM added or a guest upgraded that hasn't reported in yet, a change
  saved that wasn't applied), as the command's output says;
  `rampire apply --detailed-exitcode` exits 2 when something changed;
- the journal's event names and keys, the controller's and
  `event=operator_change` for people's changes ([journal](journal.md));
- the settings and their files ([reference](reference.md), `man
  rampire.toml`).

Text output, the TUI and the order of a command's lines are for people
and may change: a script reads `--json`, exit codes and the journal.

## Configuration in git

Keep each host's `global.toml`, `vms.d/` and `reviews/` in git and copy
them to `/etc/rampire`. Then:

```bash
rampire check                                   # the draft against the live VMs
rampire apply --yes --detailed-exitcode         # 0 unchanged, 2 changed, 1 failed
```

A draft that fails its checks changes nothing: the controller keeps
running its last applied copy, through restarts and reboots. A change to
priorities, limits or the pool reloads the controller; adding or removing
a VM restarts it. Each apply is recorded under
`/var/lib/rampire/evidence/`.

Each VM's setup is recorded on the host by `apply`, for a running VM
that names a review profile, and again when the VM changed and what the
review read from its definition still holds.

## Adding VMs

A fleet adds a VM by writing `vms.d/NAME.toml`, naming a review profile
shared by VMs of that kind, and running `apply`. The VM's definition must
already have its virtio-mem device and control channel; build them into
your VM templates, or run `rampire vm add NAME` once.

[deploy/cloud-init/rampire-guest.yaml](../deploy/cloud-init/rampire-guest.yaml)
installs the guest service when a VM is created.

## Watching

- Metrics: `/run/rampire/metrics.prom`, written every round, for
  node_exporter's textfile collector. Add the collector's user to the
  `rampire` group.
- Journal: every decision is one `key=value` line with a stable `event`
  name; see [Journal events](journal.md).
- `rampire status --json` and `rampire doctor --json` for health checks.

## Upgrading

Upgrade the controller first, then the guests. A controller serves guests
from its own minor release and the one before it; the
[changelog](../CHANGELOG.md) lists each release.
`rampire vm upgrade VM` upgrades one guest to the controller's build, and
`rampire doctor VM --json` reports a guest of another build as the
`guest build` check.
