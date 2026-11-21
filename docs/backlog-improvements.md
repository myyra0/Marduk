# Improvement backlog

Optional improvements that are not scheduled in the parity rewrite because they need separate research, new scope, or significant work. This is not a list of everything we could improve. Put small, safe improvements directly in the owning [execution packet](superpowers/plans/2026-11-20-full-parity.md) with an acceptance check. Remove an idea from this backlog once the packet takes ownership. Neither a missing parity capability nor a correctness fix belongs here.

| Idea | Area | Why | Effort (S/M/L) |
|------|------|-----|----------------|
| Benchmark independent copy-on-write clones and incremental shadow-tree rebuild | Deployment performance / `DEP-01`, `DEP-04` | May cut write amplification without making a game edit mutate the mod library. Adopt only with comparative evidence | M–L |
| Estimate required space before deploy when storage and backup costs span multiple volumes or private-view layers | Deploy planning / `DEP-02`, `DEP-03` | Useful beyond the in-scope target and backup summary, but accurate forecasts need per-method measurement and accounting | M–L |
| Optionally corroborate linked Workshop item metadata against Steam | Source provenance / `SRC-05` | Could detect removed or wrong-game items; qualify access, preserve offline linking, and never label Steam metadata as verified install or archive bytes | S–M |
