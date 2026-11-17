# JSON documents

Every JSON document RAMpire writes starts with two fields:

```json
{ "schema": "rampire.check", "version": 1, ... }
```

`schema` says what the document is, and `version` which shape it has.
RAMpire refuses to read a document whose `schema` is missing or names
another document, whose `version` it doesn't know, or that has a field
it doesn't know. Any change to a document's fields, however small, comes
with a new `version` and a note in the [changelog](../CHANGELOG.md).

A script should check both fields before it reads anything else.

## What `--json` writes

Each command's example is in [fixtures/json](../fixtures/json). A test
holds the code to it, so the examples are always what the command writes.

| Command | `schema` | Version | Example |
|---|---|---|---|
| `rampire status --json` | `rampire.status-report` | 1 | [status-report.json](../fixtures/json/status-report.json) |
| `rampire check --json` | `rampire.check` | 1 | [check.json](../fixtures/json/check.json) |
| `rampire doctor --json` | `rampire.doctor` | 1 | [doctor.json](../fixtures/json/doctor.json) |
| `rampire vm list --json` | `rampire.vm-list` | 1 | [vm-list.json](../fixtures/json/vm-list.json) |

### `rampire.status-report`

| Field | Meaning |
|---|---|
| `controller` | The controller's unit: `unit`, and `active_state`, `sub_state` and `unit_file_state` as `systemctl show` reports them (`unknown` when it reports none) |
| `vms` | One `rampire.status` document per VM, in name order |
| `unavailable` | Managed VMs whose status couldn't be read, usually because they aren't running: `vm_name`, libvirt's `state` for it (`shut off`, `paused`, or `unknown`) and the `reason` |

### `rampire.status`

One VM as the controller sees it.

| Field | Meaning |
|---|---|
| `observed_unix_millis` | When this was read |
| `vm_name`, `device_alias` | The VM and its virtio-mem device |
| `device_size_bytes`, `block_size_bytes` | The device's size and block size |
| `maximum_target_bytes` | The most the device can hold: its size in whole blocks |
| `requested_bytes`, `current_bytes` | What the device was asked for, and what the guest has plugged |
| `observe_only` | The controller decides but changes nothing |
| `guest_input` | The guest's latest publication: `error` when there is none to read, `observed_unix_millis`, and its unresolved `request` |
| `admitted_request`, `result` | The request the controller admitted and its outcome |
| `command_ownership`, `command` | Whether a resize command is in flight, and which |
| `control_health` | `converged`, `growing`, `shrinking`, `recovery_required` or `latched` |
| `actuation_latched`, `latch_reason` | Resizing stopped until a person runs `rampire recover`, and why |
| `recovery_state`, `recovery_reason`, `last_latch_clear_reason` | Recovery's state and the reasons recorded |
| `compatibility_fingerprint_sha256` | The recorded setup the controller runs on |

### `rampire.check`

| Field | Meaning |
|---|---|
| `passed` | Every VM passed |
| `vms` | Each VM's `vm_name`, and its `problem`, or `null` when it passed |

The exit code is 0 when `passed` is true and 1 otherwise.

### `rampire.doctor`

| Field | Meaning |
|---|---|
| `vm` | The VM, for `rampire doctor VM`; absent for the host |
| `passed` | Every check passed |
| `findings` | Each `check`, whether it is `ok`, the `detail` found, and the `fix` when it isn't; `run`, when the fix is one `rampire` command to run as it is, its words after `rampire` (left out otherwise). For the host, a `vm NAME` check per managed VM sums up its own |

### `rampire.vm-list`

| Field | Meaning |
|---|---|
| `vms` | Each VM on the connection, by name |
| `vm_name`, `state` | The VM, and libvirt's state for it (`running`, `shut off`, ...; `missing` for a managed VM libvirt no longer has) |
| `management` | `managed`, `unfinished` (a `vm add` that didn't apply) or `not_managed` |
| `status` | A managed VM's `rampire.status`, when it could be read |
| `detail` | One line about it for a person |
| `to_fix` | For a VM that isn't managed: what `vm add` would change |
| `suggested` | For a VM that isn't managed: the `min_ram` and `max_ram` `vm add` would give it, in bytes |
| `rampire_parts` | For a VM that isn't managed: `true` when its definition still has RAMpire's virtio-mem device or control channel, which `vm remove` takes out; left out otherwise |
| `add_restarts` | For a VM that isn't managed and runs: `true` when adding it shuts it down and starts it again; left out otherwise |
| `runs_behind` | For a managed VM that runs: `true` when it runs an older definition than its saved one, which `vm restart` brings in; left out otherwise |
| `problems` | For a VM that isn't managed: each `problem` that would still stop `vm add`, and its `fix` |
| `guest` | Its guest service: `state` is `published` with `age_seconds`, `not_published` (for a VM that isn't managed: not installed), `installed` (not managed yet, so not publishing), or `unknown` (not running, or no guest agent) |
| `os_id` | The system its definition's libosinfo metadata names, or `null` |

## Files RAMpire keeps

| File | `schema` | Version | Written by |
|---|---|---|---|
| `/var/lib/rampire/evidence/ID-apply.json`, `ID-rollback.json` | `rampire.apply-evidence` | 1 | `apply`, `rollback` |
| `attestations/VM.attestation.json` | `rampire.attestation` | 2 | `apply` |
| `/var/lib/rampire/state/VM-requests.json` | `rampire.checkpoint` | 1 | the controller |

`rampire.apply-evidence` records the `action`, `unix_millis`, the `build`
that made the change, the `draft` applied, the copies switched `from` and
`to`, every file of the new copy with its `sha256`, the `checked_vms` and
whether the `unit` was `restarted` or `reloaded` onto it.

## Between the controller and a guest

These cross the control channel, or the control files the guest agent
carries. A release's examples are in
[fixtures/protocol](../fixtures/protocol); a controller reads those of its
own minor release and the one before.

| Document | `schema` | Version | From |
|---|---|---|---|
| Publication, with the guest's request | `rampire.publication` | 1 | guest |
| Host status, with the guest's settings | `rampire.host-status` | 1 | controller |
| Retained request state | `rampire.guest-state` | 1 | guest, kept by itself |
| Windows service configuration | `rampire.windows-config` | 1 | Windows installer |
