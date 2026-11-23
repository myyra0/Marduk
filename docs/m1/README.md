# M1 — working core

M1 is the first usable slice of Marduk: one Skyrim SE installation, ZIP import, load order,
conflict preview, journaled deploy and restore, a CLI and a desktop shell.

**Status: implemented and running, but not gate-accepted.** The last independent review was a
DeepSeek pass; it blocked, and every finding it raised has been fixed with a witness or a native
run. The required independent **Sol** review has **not** run, so M1 has not been accepted.

## What works

- Import a ZIP into a private library, with the archive admitted and hashed before anything is
  published.
- Reorder mods; the lower-priority mod is overwritten by the higher one at the same path.
- Preview a deployment: per-file winner, the original that needs backing up, and any typed blocker.
- Apply and restore on Linux (tmpfs, Btrfs) and Windows (NTFS), with a journal that settles an
  interrupted operation on restart instead of guessing.
- Cross-catalog ownership: one installation is deployed by at most one catalog root at a time.
- CLI and desktop drive the same engine through the same typed API.
- Availability is evidence-backed: a route is enabled only where a recorded run proves it, and a
  guard test fails the build when code and evidence drift.

## Where it is proven

| Capability | Linux tmpfs | Linux Btrfs | Windows NTFS |
|---|---|---|---|
| Import and publish | yes | yes | yes |
| Deploy, restore, bytes and metadata | yes | yes | yes |
| Recovery across a process restart | yes | yes | yes |
| Recovery from a cut between the two Windows renames | n/a | n/a | yes |
| Installation lease, including foreign refusal | yes | yes | yes |
| Read-only original deploys and restores | yes | yes | yes |
| Launched desktop shell (smoke) | yes | — | not proven |

## What is not proven

See [limitations](limitations.md). In short: a real power cut, the stricter Windows namespace
barrier, a real Skyrim SE install, ext4, Btrfs cross-subvolume, the Windows launched shell, and an
end-to-end desktop write run.

## Review status

| Review | Result |
|---|---|
| Sol whole-packet review at `15e885b` | blocked; findings fixed |
| DeepSeek whole-packet review at `7edccc8` | blocked; finding fixed |
| Sol whole-packet review at `6df9342` | blocked; findings fixed, desktop workflow accepted as a gap; re-review pending |

The project's rule is that the final whole-packet review is a Sol pass. That pass is running against
the current commit; until it returns a verdict, no document or reply may call M1 accepted.

## The documents

| File | What it is for |
|---|---|
| [status](status.md) | The M1 task list, what is left, and the next actions |
| [limitations](limitations.md) | Accepted limitations and unproven claims, with impact and closure |
| [evidence](../research/m1-native-results.md) | The run record. Its tables are parsed by a test, so witnesses and commit revisions are normative |
| [contracts](../contracts/) | Normative behaviour: library, roots, recovery, clients |
| [ADRs](../adr/) | Decisions and their cost, including the Windows write guarantee and the installation lease |

`docs/m1/` is the place to start. The evidence file is the audit trail, not an introduction.
