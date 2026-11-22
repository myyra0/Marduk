# ADR 0004: Windows deploy uses standard filesystem flushing, without a power-cut guarantee

Date: 2026-11-22
Status: accepted
Supersedes: the Windows namespace-barrier refusal in `native_effects::durability_barrier` and the `Theoretical` barrier entry in the availability record

## Context

M1 needs deploy and restore on Windows: a mod manager that cannot place files in a game folder is not a mod manager.

Linux gives us a provable durability barrier (`fsync` of the file, then of the parent directory) whose semantics are documented and testable. Windows does not offer one we can rely on: `FlushFileBuffers` on a directory handle is not documented to commit a preceding name change, `MoveFileExW`'s write-through flag covers copy/delete, `ReplaceFileW` merges metadata we must preserve, and an ordinary token may not even hold the access the flush needs. A hard power-cut experiment could not distinguish a flushed from an unflushed directory entry, so the question is not answerable with the evidence we can produce here.

Peer mod managers accept this. Vortex copies and links with no journal and no crash recovery. MO2 either accepts the same gap or avoids writing the game folder through a virtual filesystem. Neither promises crash atomicity.

## Decision

Windows deploy and restore use standard filesystem flushing and treat a step as applied once the flush calls return:

- the staged file and its parent directory are flushed before the public effect, as on Linux;
- the effect is re-observed afterwards, and a divergence is reported as `Ambiguous` or `Conflict`, never as success;
- displaced bytes are retained exactly as on Linux, so a user edit is never destroyed;
- restart recovery re-observes recorded steps and reports what it finds. It does not claim a success it cannot prove.

**This is a narrower guarantee than Linux.** On Windows we do not promise that an interrupted deploy is always recoverable. We promise that we never claim a success we did not observe, and that we never destroy displaced bytes.

## Consequences

- Windows can deploy and restore, which is the point of the product.
- A power loss during a Windows write can leave the game install in a state that needs review; recovery will report it rather than silently repair it.
- The user-visible surface says so: the Operations panel shows a one-line note on Windows, and the README lists it under known limitations.
- Crash evidence for Windows remains unrecorded. If it is ever produced and contradicts this decision, the barrier question reopens.

## Cost if wrong

A Windows user loses a deploy to a crash and has to review or restore manually. They are told this can happen before they use it.
