# TUI functional audit

For developers. What every control of the TUI (`sudo rampire`) does,
traced from the key to the command it runs and the state it changes;
what was broken and is fixed; what is still open; and a matrix of every
operation with how it is tested. The workflows a person follows are in
[tui-workflows](tui-workflows.md); the keys are in
[reference](reference.md#the-tui).

Audited at 0.5.0, November 2026. Keep the matrix up to date with the
code: a row is **Pass** only when a test or a direct observation shows
the result, never because a handler exists.

## How an action reaches the system

Every action takes the same path, and the commands share all of it after
the TUI's part:

```text
key ─► router (tui/router.rs) ─► menu / form / dialog (tui/app.rs, form.rs, dialog.rs)
    ─► Action (tui/actions.rs) ─► task queue (tui/tasks.rs), one at a time
    ─► flow thread (tui/jobs.rs ChannelUi) ─► flows/{lifecycle,apply,controller}.rs
    ─► draft /etc/rampire, libvirt, the applied copy, systemd
    ─► Outcome ─► Ended ─► notice, tasks list, errors list
    ─► reload: inventory (vm list), the reader (pool, VM, settings), doctor/logs
```

- The TUI holds no business logic of its own for what it changes: each
  `Action` is one of the commands' flows (`vm add`, `vm remove`, `set`,
  `apply`, ...), answered through the `Ui` trait by the TUI's dialogs.
  The commands answer the same trait on the terminal.
- Reads are separate from changes. The inventory (`inventory::list`, as
  `vm list`) is read every 30 s and after every task; a reader thread
  reads the pool every second and the chosen VM, and the configuration
  every 5 s or when told to (`ReaderControl::reload`).
- Where the state comes from:

  | State | Source of truth | Read by |
  |---|---|---|
  | Managed / half-added / not managed | applied copy's `vms.d` vs the draft's `vms.d` (`inventory::Known`) | inventory |
  | A VM's settings, as running | the applied copy (`Context::running`) | reader, details panel, facts |
  | A VM's settings, to edit | the draft (`Context::draft`) | settings form |
  | Global settings, to edit | the draft's `global.toml` | global form |
  | Set up or not | the draft's `global.toml` exists | `check_setup`, at start and after `init` |
  | Guest build | the guest's publication copy | pool snapshot, `update_available` |

## What was found and fixed

Ordered by how badly it misled a person.

| # | Problem | Root cause | Fix | Test |
|---|---|---|---|---|
| F1 | Answering **No** to remove, restart, upgrade, apply, recover or init ended the task **Done** ("vm0 is removed") | The flows returned `Ok(())` for a No, and the TUI reported success from `Ok` alone | Every flow returns `flows::Outcome` (`Done`, `AlreadySo`, `Declined`, `Partly`); `Action::ended` reports from it; the commands keep exit 0 | `a_task_that_was_declined_or_left_part_done_is_not_reported_done`, `declining_changes_nothing_and_says_so` |
| F2 | A settings change answered No at its apply question said "the settings are saved and applied" | `set` saves to the draft before `apply` asks; the No was lost | `set` returns `Partly("saved in FILE, not applied: rampire apply applies it")` | as F1 |
| F3 | A `vm add` whose guest never reported in, or a `vm upgrade` whose new build never reported in, ended **Done** | The wait printed a line and returned nothing | The waits return whether the guest reported; the flows return `Partly` with the doctor command | covered by the mapping test; the wait itself needs a host |
| F4 | A `vm remove` whose apply failed (another VM failing its checks, say) left the VM's settings deleted from the draft, with the error "nothing changed" | The file was removed before `apply`, and nothing put it back | `without_settings` puts the file back on failure and says the VM is still managed; a failure after the controller has let go says the VM is no longer managed and what to run | `a_removal_the_controller_cant_take_puts_the_settings_back` |
| F5 | A removal the running controller still held said "is removed" | `still_managed_in` printed a line and returned `Ok` | Returns `Partly`, naming the copy the controller still runs | needs a host |
| F6 | A half-added VM (an add that stopped after writing its settings) could be finished but not undone from its menu, though `vm remove` undoes it | The menu for `Unfinished` had no `x` | `x` "Undo the add: remove RAMpire" | `a_half_added_vm_can_be_finished_or_undone_from_its_menu` |
| F7 | A VM's settings form showed the applied values but `set` edits the draft. With a change saved but not applied, the form showed the old value, and saving another field silently applied the hidden change | Two sources of truth: the form read the reader's `details.settings` (applied copy) | The form reads the draft; both settings forms warn when the draft differs from what runs | `the_menus_letters_work_from_the_list` |
| F8 | `s` on a VM just chosen said "still being read; try again in a moment" | Same as F7: it waited for the reader | Reading the draft is immediate; a file that can't be read is named with its error | as F7 |
| F9 | Nothing in the TUI applied a draft left unapplied (F2), except a doctor fix | No control for `apply` | `A` anywhere runs `rampire apply`, which shows the difference and asks | `a_draft_saved_but_not_applied_can_be_applied_from_anywhere` |
| F10 | The **Done** notice showed the command line, not what was done | `on_task` dropped `Ended::Done`'s text | The notice shows the text | as F1 |
| F11 | `u` with VMs marked upgraded only the VM under the cursor | Marked letters were `asx` only | `asxu` | `marked_vms_are_upgraded_each_not_the_hovered_one_alone` |
| F12 | Removing a VM from its live details left the screen on a VM that is no longer managed | Nothing left the view | A successful removal of the VM shown returns to the list | `a_vm_removed_from_its_details_goes_back_to_the_list` |
| F13 | With half-added and new VMs marked, `a` opened the add form and silently dropped the half-added ones | `for_marked` returned early | It says which were left out and how to finish them | none (a notice) |
| F14 | `I` on a set-up host pointed to a command for what `S` does | Message text | "S changes its global settings" | none (a notice) |

## What is still open

Each needs a change not made in this audit; the reason is given.

| # | Problem | What is needed |
|---|---|---|
| O1 | The guest login given to `vm add` (`--guest-login`, the add form's Guest login) is not kept, so `u` (upgrade) can only reach the guest through its agent or as root at its address, then offers to shut it down | Keep the login in the VM's file, or ask for it in the upgrade task when the automatic routes fail |
| O2 | Restart (`vm restart`) and recover (`recover`) are reached only from the doctor's fix (`Enter` on the finding) | Deliberate: the doctor finds when each is needed. Offer restart in the menu when a VM runs behind its saved definition, which the inventory doesn't read yet |
| O3 | `rollback` and `diff` have no control; `diff` is shown only inside apply's question | Add when there is a need; `A` shows the difference before asking |
| O4 | A running task is cancelled only at its next question. The long waits (booting a VM to log in, up to 3 min; the guest reporting in, up to 5 min) can't be interrupted | Cancellation checked inside the waits |
| O5 | Quitting with a task running ends it mid-step. The quit question warns; a `vm add` stopped after changing the definition is then half-added or not managed with RAMpire's parts, and `x` undoes either | The add undoing itself ([roadmap](roadmap.md#adding-vms)) |
| O6 | The commands exit 0 for a No and for a part left (`Outcome::Declined`, `Partly`), as before, so a script can't tell them from success | Owner decision: a distinct exit code changes what scripts see |
| O7 | Whether a VM can take the guest package shows only when Add runs | [Roadmap](roadmap.md#adding-vms): inspect disks in the background |
| O8 | `tui/app.rs::restarts` decides from `vm list`'s change text (`starts_with("save")`) whether adding restarts a VM, the same rule `vm add` applies in `lifecycle.rs`: two copies of one rule, joined by wording | One function in `vm.rs` that both call |
| O9 | `actions::from_fix` finds the command in a doctor fix's text | The doctor's findings carry the action as data |
| O10 | `init` on a set-up host reports "set up already" even when it starts a stopped controller | `start_controller` says whether it started it |

## Architecture

What works and should stay:

- One key table (`keymap.rs`) feeds dispatch, help, hints and the
  palette, so a key can't be shown and do something else.
- One question interface (`ui::Ui`) for the commands and the TUI: the
  TUI runs the commands' own flows, so there is one implementation of
  each operation.
- Tasks run one at a time, each on its own thread, and every question
  goes through a channel; a question can wait while the person does
  something else.

What made the TUI unreliable, and how it is now:

- **Success read from `Ok`.** A flow returning normally was taken for
  done (F1 to F5). `flows::Outcome` makes how it ended part of the
  return type, so a new flow can't be reported done by accident.
- **Two sources of truth for one form.** The VM form read the applied
  copy and wrote the draft (F7). Forms now read what they write, and
  views read what runs.
- **Side effects before a question or a check with no way back.**
  `vm remove` deleted first and checked after (F4). It now puts the file
  back. `set` was already safe: `set::set` checks the whole
  configuration before replacing a file, and puts it back if the
  configuration doesn't load.

Not changed, worth knowing:

- `App` holds every layer's state and most handlers (about 1400 lines in
  `app.rs`, 1200 in `router.rs`). It is coupled but not tangled: each
  layer's handler is its own function, and every screen is drawn from
  `App` alone, which is what makes the key-press tests possible.
- Errors are `String` throughout the flows. The TUI shows them whole, in
  the task and the errors list, so nothing is lost, but a caller can't
  match on a kind of error.

## How it is tested

| Level | What | Where |
|---|---|---|
| Unit | Keys, chords, layers (`keymap`); fields, ranges, limits against each other (`form`); text fields (`input`); scrolling; themes; command lines of every action; doctor fixes read as actions | `tui/*.rs` tests |
| Integration, TUI to flow | A task's question comes to a dialog and the answer goes back (`jobs`); an outcome becomes the right ending and notice (`router`); declining a flow changes nothing (`lifecycle`); a removal that fails puts the settings back (`lifecycle`); settings changes keep the file's comments and write nothing invalid (`set`) | as named in the matrix |
| Workflow, by key | `App::on_key` pressed as a person would, with `hold_tasks` so the command is queued but not run: menus, marks, forms, the palette, the help, quitting | `tui/router.rs` tests |
| Drawing | Every view and layer at 80x24, 120x40 and 200x60, every theme, colour depth and icon set | `tui/views/mod.rs` |
| End to end | The TUI on a host with libvirt VMs, doing each task and reading the result back with the commands | Not automated. Needs a deployment ([development](development.md#platform-specific-checks)) |

The tests never run a flow against libvirt, systemd or `/etc/rampire`:
`hold_tasks` stops a queued task, and flow tests use a temporary draft
and stop at the first question. So **mocked success is not proof**: every
row whose result is a change on the host is **Blocked** below until it is
done on a deployment.

To verify the blocked rows: install the current build on a test host
(`tools/build.sh install`, with the owner's agreement), with at least two
disposable VMs (one Debian or Fedora VM with `qemu-guest-agent`, one plain
VM), then follow each workflow in [tui-workflows](tui-workflows.md) and
check the result with `rampire vm list`, `rampire status VM`, `rampire
diff` and `virsh dumpxml --inactive VM`.

## The matrix

Coverage: **U** unit, **I** integration (TUI to flow, or flow on a
temporary draft), **K** key-press workflow, **D** drawing, **E** end to
end. Verified: **Pass** by the test or observation named; **Blocked**
needs a host; **Untested**.

### The list and a VM's menu

| Screen | Action | Expected result | Backend | Status | Coverage | Verified | UX issues | Required changes |
|---|---|---|---|---|---|---|---|---|
| List | Move, `/` filter, sort `,`, columns `m`, mark Space / Ctrl+A | List narrows, orders, marks | `App::visible` | Complete | U K D | Pass (`the_filter_narrows…`, `chords_sort…`, `the_list_filters_and_sorts…`) | | |
| List | Enter: menu | Only what fits the VM, by management | `App::menu` | Complete | K D | Pass (`enter_opens_a_menu…`, `a_half_added_vm…`) | | |
| List | Tab: facts | `vm list`, pool and settings facts; `y` copies | `spot::facts` | Complete | K | Pass (`spot_shows_a_vms_facts…`) | | |
| Managed | `v` live details | `rampire watch VM` live | `watch::Command`, reader | Complete | K D | Pass (drawing); live readings Blocked | | |
| Managed | `s` settings, one VM | Form with the draft's values; warning if not applied; Enter runs `set VM K=V...` for what changed, then apply asks | `lifecycle::set` | Complete (F7, F8) | K I | Pass (form opens with draft values); the change on the host Blocked | | |
| Managed | `s` with VMs marked | One form, empty fields leave each its own; a `set` task per VM | `lifecycle::set` | Complete | K U | Pass (`the_form_changes_only_what_was_edited`); host Blocked | | |
| Managed | Settings: invalid value | Red with the reason; Enter goes to the first wrong field | `Form::problems` | Complete | U | Pass (`what_is_typed_is_checked…`) | | |
| Managed | Settings: No at apply | **Not done**: saved in the draft, not applied | `set` → `Outcome::Partly` | Complete (F2) | I | Pass (mapping) | | |
| Managed | Settings: `max_ram` above the device | Device grows in the definition; restart asked | `grow_for_max_ram` | Complete | none | Blocked | | |
| Managed | `o` doctor, `l` logs | That VM's findings, its journal lines | `doctor_vm`, `journal::read_log` | Complete | K | Pass (`the_menus_letters_work…`); content Blocked | | |
| Managed | `u` upgrade guest | Shown only with an update; asks; installs; waits for the new build; **Not done** if it doesn't report | `vm_upgrade` | Complete (F3); login not kept (O1) | K | Pass (menu, queue); install Blocked | Can't give a login | O1 |
| Managed | `x` remove | Asks; settings out; apply; plain RAM definition; RAMpire's files removed | `vm_remove` | Complete (F1, F4, F5) | I K | Pass (declined, put back on failure, queued); removal on the host Blocked | | |
| Managed, missing from libvirt | `x` remove | Settings out, applied, files removed; definition skipped | `vm_remove` | Complete | none | Blocked | | |
| Not managed | `a` install | Add form, suggested sizes, running warning; Enter: `vm add` shows its plan and asks once, review questions, install, apply, wait | `vm_add` | Complete (F3) | K U | Pass (form, command line, warning); the add Blocked | Can't-install systems found only at Add | O7 |
| Not managed | `a`, No at the plan | **Not done**, nothing changed | `vm_add` → `Declined` | Complete | I | Pass (mapping) | | |
| Not managed, RAMpire's parts left | `x` remove | Definition to plain RAM, files removed | `clear_definition` | Complete | I | Pass (fails before asking without libvirt); host Blocked | | |
| Half-added | `a` finish | `vm add` with the kept settings | `vm_add` (resuming) | Complete | K | Pass (queued); host Blocked | | |
| Half-added | `x` undo | Settings out; old definition back | `vm_remove` | Complete (F6) | K | Pass (queued); host Blocked | | |
| List, marked | `a`, `x`, `u` | One question listing the commands; a task each | `for_marked` | Complete (F11, F13) | K | Pass (`marked_vms_get_one_question…`, `marked_vms_are_upgraded_each…`) | | |
| Menu | Shift+letter | Copies the command line | `which` | Complete | K | Pass | | |

### Anywhere

| Screen | Action | Expected result | Backend | Status | Coverage | Verified | UX issues | Required changes |
|---|---|---|---|---|---|---|---|---|
| Start, host not set up | Setup form | Recommended reserve, pool shown; Enter runs `init --host-reserve` | `controller::init` | Complete | U | Pass (form); init Blocked | | |
| Anywhere | `I` | Setup form, or says `S` | `setup_form` | Complete (F14) | none | Untested (a notice) | | |
| Anywhere | `S` global settings | Draft's `global.toml`, every setting; warning if not applied; Enter runs `set global` | `lifecycle::set` | Complete | U | Pass (`the_global_form_checks…`); host Blocked | | |
| Anywhere | `A` apply | Difference shown, asked; **Done**, **Done: nothing to apply**, or **Not done** | `apply::apply` | Complete (F9) | K | Pass (queued); apply Blocked | | |
| Anywhere | `d`, `g d` doctor | Host and every VM; Enter runs a fix that is a command, after a question | `jobs::doctor`, `from_fix` | Complete | U D | Pass (`a_fix_that_is_a_whole_command…`, `the_doctor_offers…`); findings Blocked | Fix found in text | O9 |
| Doctor | Enter on a fix: restart, recover | `vm restart` asks; `recover` asks why, then asks | `vm_restart`, `recover` | Complete (F1) | I | Pass (restart declined); host Blocked | Only from the doctor | O2 |
| Anywhere | `l`, `g l` logs | Journal, find, one VM | `journal::read_log` | Complete | K | Pass (`the_logs_find…`); journal Blocked | | |
| Anywhere | `w` tasks; Enter, `x` cancel, `D` clear | Output, question; cancel waiting at once, running at its next question | `tasks` | Complete | K U | Pass (`cancelling_drops…`, `a_tasks_question_comes_forward…`) | Long waits can't be cancelled | O4 |
| Task | `y`/`n`/Enter, text | Answer goes to the flow; Esc leaves it waiting | `ChannelUi` | Complete | I K | Pass (`a_flows_questions_come_back…`) | | |
| Task ends | Notice and state | **Done** with what was done; **Not done** with what wasn't; **Failed** in `e` | `Action::ended` | Complete (F1, F10) | K | Pass | | |
| Anywhere | `e` errors | Every failure kept, `y` copies | `errors` | Complete | K | Pass | | |
| Anywhere | Ctrl+P / `:` palette, `?` help | Find a command or key; Enter does it | `palette`, `help` | Complete | K U | Pass | | |
| Anywhere | `r` read again | Inventory, guest probes, reader; doctor or logs again | `reload` | Complete | none | Untested | | |
| Anywhere | `t`, `i` | Theme, icons | `theme` | Complete | U D | Pass | | |
| Anywhere | `y` copy | OSC 52 to the person's clipboard | `term::copy` | Complete | U | Pass (sequence); a real terminal Blocked | | |
| Anywhere | `q`, Ctrl+C | Asks while tasks run; Ctrl+C twice quits | `ask_quit` | Complete | K | Pass | Task ends mid-step | O5 |
| Anywhere | Someone else's change | Notice with who; reads again | `peers` | Complete | U | Pass (parsing); the journal Blocked | | |
| Anywhere | After any task | Inventory and configuration read again; doctor and logs reloaded | `on_task` | Complete | K | Pass (state); reads Blocked | | |
