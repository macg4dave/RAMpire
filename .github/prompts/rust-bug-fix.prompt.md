---
mode: agent
description: Find the cause of a bug, fix it and make sure it can't come back
---

Bug: `${input:bug:what happened, what was done just before, what was expected}`

1. Reproduce it first, at the lowest layer that shows it
   ([testing](../../docs/testing.md)): a unit test, the simulated host,
   or the live suite. The test fails for the reported reason before any
   fix.
2. Find the cause, not the symptom. Look for the same mistake in the code
   beside it and in other flows that share the path.
3. Fix it, then run the test, the gate, and the layer that showed it.
4. Say what the cause was, what the test checks, and what wasn't verified.
