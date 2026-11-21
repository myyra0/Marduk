# Resolve deployment before selecting how files are presented

**Status: proposed.** Use one deterministic resolution for conflict inspection, file-map export, physical placement and private game views. Compute and review logical target intents before selecting the per-platform effect path. This keeps game rules and provenance consistent while allowing platform-specific mechanics, instead of putting a shared virtual filesystem beneath every operation.

Keep copy, hardlink, symlink and private view user-selectable where supported. M1 uses copies to prove recovery. Linux private view starts with a shadow-tree implementation and earns an OverlayFS/FUSE alternative by evidence. Windows tries relocation first, then evaluates a compatible existing runtime only for workflows with a canonical-path requirement. An unavailable private view never implicitly becomes a physical deployment. [Comparative evidence and decision criteria](../research/deployment.md).
