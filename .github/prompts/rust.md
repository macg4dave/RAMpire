# Rust standards

The rules for this repository's Rust that a capable Rust programmer
wouldn't follow by default. For new and changed code; existing code that
breaks one isn't a reason for an unrelated rewrite.

## Memory and control

- Memory quantities are `u64` bytes. Keep whole-guest RAM, boot (fixed)
  RAM and the virtio-mem device's allocation apart. Use checked
  arithmetic; saturate only for a documented floor or ceiling. Zero is a
  size, never "not set": use `Option`.
- Validate settings, protocol data and saved state where they enter.
  Keep the identity, freshness, replay and ordering checks.
- A request or command whose outcome is unknown stays unknown: never
  replay it silently, drop unresolved state or clear a latch. Read
  changing state again just before acting on it.

## Failures

- No `unwrap`, `expect`, `panic!`, `todo!` or `dbg!` outside tests.
- An error carries what was being done and its cause, in words a person
  running the command understands, with what to do about it where there
  is something.
- No `let _ =` or `.ok()` that drops an error without a comment saying
  why it is safe to. Never report work that didn't finish as done; the
  commands' exit codes are in [reference](../../docs/reference.md).

## Platform code

- `unsafe` only at OS and FFI boundaries, each block with a `SAFETY:`
  comment on the pointers, lifetimes and handles it relies on.
- A thread or callback never outlives the state it uses.
- Anything that could write to the terminal while the TUI is open goes
  through the TUI, never to stdout or stderr.

## Lints and dependencies

- Clippy warnings are denied by the gate. No `#[allow]`: for a lint that
  really doesn't apply, `#[expect(clippy::name, reason = "...")]` on the
  smallest item.
- A new crate needs a reason the standard library or an existing
  dependency can't meet; turn off its unused features.

## Writing

- British English in prose, comments and messages. Keep the spelling of
  APIs, protocol fields and anything machine-readable as it is.
- Comments say why, and the contracts code relies on; not what the next
  line does.
