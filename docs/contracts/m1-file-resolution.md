# M1-C01: priority resolution contract

**Status:** Proposed worker brief contract, not implementation or execution approval. [M1-C01](../superpowers/plans/2026-11-20-m1-working-core.md#m1-c01--workspace-shared-domain-and-resolver) owns these files and tests. Its inputs are synthetic archive manifests; OS and archive readers are not dependencies of the resolver.

## Module ownership and interface

- `ids.rs`: local opaque `u64` newtypes for `ProfileId`, `ProfileRevision`, `MemberId` and `InstalledRevisionId` with equality, ordering and copying. Each exposes `new(u64) -> Self` and `get(self) -> u64`. The storage packet owns allocation/serialization; numeric order never determines priority.
- `profiles.rs`: `ProfileSnapshot { id: ProfileId, revision: ProfileRevision, members: Vec<ProfileMember> }`; `ProfileMember { id: MemberId, installed: InstalledRevisionId, enabled: bool }`. Membership vector is low to high.
- `targets.rs`: validated M1 `RelativePath`, `TargetFile { root: RootRole, relative: RelativePath }`, initial `RootRole::Data`. `RelativePath::parse(&str) -> Result<RelativePath, PathError>` preserves spelling; `as_str(&self) -> &str` reads it. `PathError` has `EmptyPath` and `InvalidComponent { index: usize, reason: PathViolation }` (zero-based); `PathViolation` is `Empty`, `Dot`, `ForbiddenCharacter`, `TrailingDotOrSpace`, or `ReservedDeviceName`. Reject the first invalid component. Add a target-role variant only when an approved later packet consumes it.
- `resolve.rs`: accepts borrowed `ProfileSnapshot` and `ResolvedInputs`. No persistence, filesystem, archive decompression or network dependency. `RelativePath` bars unadmitted spellings; R01 separately verifies actual roots and directory aliases.

```rust
pub struct ResolvedInputs {
    pub manifests: BTreeMap<InstalledRevisionId, Vec<TargetFile>>,
}

pub struct ResolvedFiles {
    pub revision: ProfileRevision,
    pub entries: Vec<ResolvedEntry>, // canonical target order
}

pub struct ResolvedEntry {
    pub target: TargetFile,              // winning spelling, before native placement
    pub candidates: Vec<FileCandidate>,  // low to high, winner last
}

pub struct FileCandidate {
    pub member: MemberId,
    pub installed: InstalledRevisionId,
    pub input: TargetFile,
}

pub enum ResolveError {
    DuplicateMember(MemberId),
    MissingManifest(InstalledRevisionId),
    AliasedEntries {
        installed: InstalledRevisionId, first: TargetFile, second: TargetFile,
    },
    ConflictingTargetShape { first: TargetFile, second: TargetFile },
}

pub fn resolve_files(
    profile: &ProfileSnapshot,
    inputs: &ResolvedInputs,
) -> Result<ResolvedFiles, ResolveError>;
```

`TargetFile` is a normalized **logical** destination, not a disk path or an archive entry locator. M1 synthetic manifests use source paths equal to destination paths under `Data`, so revision plus `input` identifies the approved file. M2 may extend this to explicit per-entry source locators when routing/stripping needs it. Candidate information is not mutable inventory; the caller must revalidate the profile revision and installed sources before applying any plan.

## Required behavior

1. First reject duplicate membership IDs, including disabled memberships, in vector order. Then ignore disabled members' manifest omissions and file entries. An enabled member must have a manifest even if it is empty. Two different membership IDs may refer to the same installed revision; provenance still includes both.
2. Admission uses the [M1 path policy](../superpowers/plans/2026-11-20-m1-working-core.md#m1-r01--root-identity-portable-relative-paths-and-safe-publication-seam): slash-separated portable ASCII; no invalid or reserved components. Compare logical file targets by root role and ASCII-lowercased components. Preserve the winning spelling in output. Do not invent Windows/Unix runtime identity from this comparison.
3. Two files in the *same* enabled manifest with equal logical target keys produce `AliasedEntries`, including exact duplicates. Different mods supplying equivalent paths produce one normal conflict. Append candidate rows following the member vector, not IDs or lexical names.
4. File-vs-directory ancestor conflicts like `textures/pack` versus `textures/pack/file.dds` produce `ConflictingTargetShape`, even across different mods. There is no legal placement for both in M1; don't hide one by applying priority. The native adapter independently refuses an incompatible existing disk object or a race.
5. On success sort entries lexicographically by the canonical target key and preserve input revision. Return no partial generation on error. File bytes and active profile state never change during resolution.

Path admission is deliberately narrower than underlying filesystem support. Check an empty whole string first. For each slash-delimited component, in order: reject empty, `.` or `..`, bytes outside printable ASCII (`0x20..=0x7e`) or any of `< > : " \ | ? *`, then a trailing dot or space, then reserved basenames. Compare the part before the first dot ASCII-insensitively after trimming trailing ASCII spaces; disallow `CON`, `PRN`, `AUX`, `NUL`, `CONIN$`, `CONOUT$`, `COM1`–`COM9` and `LPT1`–`LPT9` with or without an extension. Native limits, links, volume/external aliasing and length are R01 checks instead of claims about this function.

## Planner test inputs and witnesses

Use the exact [golden fixture](../research/recovery-oracle.md#golden-fixture) for names and expected bytes in end-to-end tests. M1-C01's `crates/core/tests/priority.rs` works on manifests, not disk bytes.

| Test | Setup | Assert |
|---|---|---|
| `later_enabled_member_wins_and_keeps_provenance` | Members A=`11`, B=`7`; both enabled and supply `textures/winner.dds`; A also has `textures/a-only.dds`; revision=`8` | A-only sorts first; overlap lists `[11,7]` and B wins; reorder memberships `[B,A]` and A wins with `[7,11]`. Result revision equals snapshot each time. |
| `disabled_member_does_not_compete` | A and disabled B; no manifest for B | Only A is a candidate; adding the B manifest doesn't change output. If B is then enabled without a manifest, get `MissingManifest`. |
| `same_mod_alias_is_invalid_but_overlap_is_allowed` | One member has `Textures/Winner.dds` and `textures/winner.dds`; then split the two entries across A and B | First `AliasedEntries`, then a single conflicting target with winner spelling from B. |
| `file_directory_overlap_has_no_plan` | A supplies `textures/pack`, B supplies `Textures/pack/f.dds` | `ConflictingTargetShape`, not a partial generation or a winner chosen by priority. |
| `duplicate_member_has_no_plan` | Same member ID added twice with one disabled | `DuplicateMember`; manifest need not be read to diagnose malformed membership identity. |

Run `cargo test -p marduk-core --test priority` once the approved skeleton exists. The first behavioral run must reach the assertions and fail because required resolution is not implemented, not because the toolchain or fixtures are missing. Keep the initial test result and rerun the same checks after implementation. Validation of `RelativePath` needs focused boundary tests for valid ASCII, separator/dot, device basename even with an extension, and trailing-dot/space cases.

## Review stop conditions

Stop and return evidence if a worker needs real filesystem calls in the resolver, needs to change the provided test oracle, needs to make one mod win a file/directory shape conflict, or discovers that a product requirement needs a source locator broader than the chosen M1 scope. None is a garden-variety implementation fix.
