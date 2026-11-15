# TUI workflows

For anyone running RAMpire from its TUI (`sudo rampire`). Each workflow
says where to start, which keys to press, what you are asked, and what
is true on the host when it ends, with the command that checks it. The
keys are listed in [reference](reference.md#the-tui); how each control
is built and tested is in [tui-functional-audit](tui-functional-audit.md).

Every action runs as a task in the background (`w` lists them) and ends
in one of three ways:

| Ends | Means |
|---|---|
| **Done** | It did what it was started for, or that was so already. The notice says which |
| **Not done** | You answered No, or part of it is left. It says what was and wasn't changed, and what finishes it |
| **Failed** | It stopped on an error, kept in the errors list (`e`). It says what it changed before the error, if anything, and how to go on or go back |

## Set up a host

1. Open the TUI on a host without `/etc/rampire/global.toml`.
   **Set up this host** opens first, with the recommended host reserve.
2. Keep or change the reserve; the pool it leaves shows as you type.
   Enter runs `rampire init --host-reserve SIZE`.
3. The task checks the host, writes `global.toml`, shows what it applies
   and asks; Yes applies and starts the controller.

Ends **Done** when it is applied and the controller runs; **Not done**
when you answer No at the apply question: `global.toml` is written but
nothing runs. **A** applies it later. Check: `systemctl status rampire`,
`rampire diff`.

Esc closes the form; **I** opens it again. On a host that is set up,
**S** changes the same settings.

## Add a VM

1. In the list, choose a VM marked **not managed** and press Enter, then
   `a` (or `a` from the list). The menu shows what would stop adding it
   first.
2. The form has its settings, filled in from its definition: Priority,
   Minimum, Maximum and Free RAM, and Guest login (empty: the guest agent,
   or root at its address). A warning says when the VM is running and
   will be shut down. Enter goes on.
3. The task shows everything it will do and asks once. Then the review
   questions it can't read from the definition, then it changes the
   definition, starts or restarts the VM, installs rampire-guest, writes
   the VM's settings and its review, applies, and waits up to five
   minutes for the guest to report in.

Ends **Done** when the guest reports in; **Not done** if you answered
No (nothing changed) or if the guest hasn't reported in yet (the VM is
managed: `o` shows why). Check: `rampire vm list`, `rampire status VM`.

If it fails after changing the definition, the error says so, and the VM
shows as **half-added** (its settings are written) or **not managed**
with RAMpire's parts in its definition. Both menus have `x` to put the
old definition back; a half-added VM's `a` finishes the add instead.

A VM that runs a system without dnf or apt (Home Assistant OS, an
appliance) is refused before anything is changed.

Several VMs: mark them with Space, then `a`: one form, whose empty
fields give each VM its own suggested size, and a task for each.

## Change a VM's settings

1. Choose a managed VM, Enter, `s` (or `s` from the list or its
   details).
2. The form shows the settings in the draft, `/etc/rampire/vms.d/VM.toml`.
   If the draft has changes that aren't applied yet, it says so: saving
   applies them too. Wrong values are marked red with the reason.
3. Enter runs `rampire set VM KEY=VALUE` for what you changed. When the
   new Maximum RAM is more than the VM's memory device, the device grows
   in its definition, and you are asked whether to restart now. When its
   guest runs another build, it is upgraded too. Then the difference is
   shown and you are asked to apply.

Ends **Done** when it is applied; **Not done** when you answer No to
applying: the change is saved in the draft only, and **A** applies it.
Check: `rampire diff` (nothing left), the details' settings panel.

Several VMs: mark them, `s`: empty fields leave each VM its own value.

## Change the global settings

**S** anywhere. The form shows every setting in the draft's
`global.toml`, defaults included, and says when the draft has changes not
yet applied. Enter runs `rampire set global KEY=VALUE` for what changed,
then shows the difference and asks to apply. Ends as for a VM's
settings.

## Apply what was saved

**A** anywhere runs `rampire apply`: the difference between the draft
and what runs, and a question. Ends **Done** when applied, **Done**
saying "nothing to apply" when the draft is what runs, **Not done** on
No.

## Upgrade a guest service

A VM whose guest runs another build than the host shows **update
available (u)**. Press `u` on it, or mark several and press `u`. The task
asks, installs the host's build in the VM through its guest agent or as
root at its address, and waits for the new build to report in. When it
can't log in, it asks whether to shut the VM down for a moment to put the
package on its disk.

Ends **Done** when the new build reports in; **Not done** on No or when
it hasn't reported in yet. Check: `rampire vm list`.

The TUI can't give a login other than root; for a VM added with
`--guest-login`, use `rampire vm upgrade VM --guest-login USER@HOST`.

## Remove RAMpire from a VM

Removing is not deleting: the VM, its disks and its data stay. What goes
is RAMpire's management of it.

1. Choose the VM, Enter, `x` (or `x` from the list or its details).
2. The task asks. Yes takes the VM's settings out of the draft and
   applies, so the controller lets go of it; puts its definition back to
   plain RAM (its boot RAM, or its definition before it was added) from
   its next start; and removes what RAMpire kept for it: its recorded
   setup, request state, definition backups, and its review when it was
   written for it alone.

Ends **Done** when all of that is done, and a list shows the VM as **not
managed**; **Not done** on No (nothing changed), or when the controller
still runs an older applied copy that has the VM (what RAMpire kept stays
until it moves on). **Failed** when applying fails: the settings are put
back, so the VM is still managed as before. A failure after the
controller has let go says the VM is no longer managed and what is left;
while its definition still has RAMpire's parts, its menu offers `x`
again. Check: `rampire vm list`,
`virsh dumpxml --inactive VM`.

From a VM's details, a successful removal goes back to the list.

| The VM is | `x` does |
|---|---|
| Managed | All of the above |
| Managed, but gone from libvirt | Takes it out of the configuration and removes its files; there is no definition to change |
| Half-added | Undoes the add: settings out, old definition back |
| Not managed, with RAMpire's parts | Puts the definition back to plain RAM and removes its files |
| Not managed, plain | Not offered |

## Check and fix

**d** shows the doctor for the host and every VM; `o` in a VM's menu,
that VM's. `p` shows only the problems, `/` filters. Enter on a finding
whose fix is a `rampire` command runs it, after a question: `apply`,
`vm restart VM` (a VM that runs behind its saved definition), `recover
VM` (a latched VM: you are asked why it is safe), and others. A fix done
by hand is shown under the problem; `y` copies it.

## Watch and read

- **v** in a managed VM's menu: its details, live (`rampire watch VM`).
  Left and Right go through the VMs.
- **Tab**: a VM's facts.
- **l**: the journal; `/` finds, `n` and `N` go between the lines found,
  `f` shows one VM's lines.
- **r** reads everything again. The list is read again every 30 s and
  after every task, and the pool every second.

## Tasks, questions and quitting

- A task's question comes forward when nothing else is open; Esc leaves
  it waiting, and the status bar counts it. `w`, Enter answers it.
- `x` in the tasks cancels: a waiting task at once, a running one at its
  next question, changing nothing more. A step without a question, such
  as waiting for a VM to boot, runs to its end.
- `q` asks while tasks run. Quitting stops them where they are; a VM add
  stopped part-way is finished or undone from its menu.
