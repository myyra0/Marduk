# M2-I01: local archive admission and install

**Status:** Draft. [M1-C02](m1-zip-library.md) owns baseline ZIP safety and publication. [Archive trace](../research/archive-crosswalk.md) is evidence for candidate coverage, not a shipped-format list.

## Freeze one reader per qualified variant

| Family | Baseline / M2 qualification grid |
|---|---|
| ZIP | Keep M1 stored/deflated classic ZIP and content detection regardless of filename. Qualify ZIP64 and observed extra methods, split, encryption and filename encodings separately. Do not change the M1 reader merely to unify libraries. |
| 7z | Start with single-volume, unencrypted LZMA/LZMA2. Test solid and non-solid separately; qualify exact methods and bound dictionary, solid-block expansion and metadata before allocation. |
| RAR | Qualify RAR4 and RAR5 distinctly with single-volume unencrypted fixtures. Solid, encrypted and multi-volume inputs need separate evidence for listing and extraction. The pinned listing rejects password-protected RAR; that is not evidence about every install path. Do not make a restricted helper a dependency without rights and package review. |
| TAR | Qualify plain, gzip, bzip2, xz and zstd streams separately, including evidence that the decoded stream is actually TAR. Test harmless supported directory/filename metadata apart from links, devices, sparse files and other special semantics. A bare compression header does not admit an archive. |

Record each relevant variant as admitted, proven not a retained route, or blocked with evidence and owner. A recognized but unqualified variant returns `UnsupportedContainer` or `UnsupportedEntry`; it is not counted as supported parity. Do not drop a working pinned route because a chosen reader lacks it. Prefer bounded in-process readers; a helper needs deliberate review of isolation, precise resource limits, entry-to-output correspondence, distribution rights and executable/package identity on Linux and Windows. The pinned recipes reference both `py7zr` and packaged tools such as `bsdtar`; neither establishes runtime coverage or justifies copying the fallback chain.

## Entry and publication contract

- Use the M1 private source snapshot and publish-by-catalog protocol. Identify a container by content and verified structure; let exactly one approved reader handle any admitted variant. After reader selection, corruption or a non-format failure never dispatches a different extractor.
- Expose an ordered admitted-entry manifest and opaque entry identity scoped to the snapshot. A sequential reader may list and stream in separate passes from the same snapshot; do not promise random access. Keep original entry identity distinct from the logical destination, so staged output and later interactive choices can refer to the same exact entry.
- Reject ambiguous layouts, traversal, absolute paths, embedded links/special entries and conflicting names before publication. An approved path compatibility transform runs **before** validation/collision checks; retain the raw entry identity and validate again after any reviewed layout mapping. Resolve auto-suggestions only for the verified handler layout; at most one generic outer wrapper is suggested. Keep the accepted source-to-destination mapping in the reviewed import. Ambiguous suggestions require an explicit choice, not recursive stripping.
- Inherit M1's bound dimensions and cap both inventory and emitted output independently. Freeze per-variant defaults, decoder-memory budgets and stop behavior before implementation; use lower fixture limits to exercise exact boundaries. Unsupported names/metadata are reported, never rewritten just to get past validation. Keep regular-file-only staging except for handled structural directories.
- Reuse explicit Append/Replace; a matching name never targets a member. Reinstall binds the recorded exact archive ID/digest, member and revision, not a same-name/same-size file or a future provider resolution. If the source is gone, ask for an exact replacement source or defer provider reacquisition to M3. Publish new bytes by new revision; retain the prior revision during any failed/interrupted replace. An input chosen by picker/drop is never deleted as install cleanup.

## Fixture handoff

| Check | Required distinguishing result |
|---|---|
| `each_advertised_variant_lists_extracts_and_installs` | Per qualified variant, same `textures/winner.dds = b\n` fixture, including valid content under a wrong suffix. Assert one ordered admitted entry, streamed bytes, committed revision and source left intact. Header, magic match or successful list alone fails the gate. |
| `mapping_preserves_meaningful_subtrees` | Synthetic handler recognizes `bin/x64/loader.dat = stage\n`. Archive with `Wrapper/bin/x64/loader.dat` suggests removing only `Wrapper`; archive rooted at `bin/x64/loader.dat` removes nothing. Change the reviewed mapping or target revision before Apply and assert no publication. |
| `invalid_or_over_budget_entry_cannot_publish_partial_mod` | Include an accepted file followed by a truncated, over-budget or unsupported entry; run unsafe absolute path, parent traversal, link and duplicate cases per reader. On enabled separator normalization, `a/b` plus `a\b` collides. Assert old bytes and profile unchanged, no outside write, no `Prepared`/published mod and no fallback reader call. |
| `reinstall_requires_exact_source_and_keeps_previous_on_failure` | Wrong archive with identical filename/length is rejected; interrupted or cancelled replace leaves the same prior member and bytes. Successful replacement preserves member identity, position and enabled state. |

Start with `cargo test -p marduk-archive --test local_install_contract` (proposed); freeze exact archive fixture bytes/digests, storage checks and reader versions/features after M1-C02 plus G-PATHS/G-LICENSE review. Do not reuse production reader logic to generate negative fixture expectations. M3-I02 owns nested interactive containers and prompts; their budget must include the parent archive. M3-B01 owns game archive readers/writers. Neither expands this allowlist by implication.
