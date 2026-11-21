# Journal external effects before publication

**Status: proposed.** Keep one local metadata and operation ledger database, but treat the journal and actual game files as separate durability domains. Record exact target identity and intent, preserve durable pre-images before mutation, and reconcile observed old/new/unknown state after interruption. Neither a successful metadata commit nor a successful file move proves the other persisted.

The alternative of “replace, then save the deployed list” cannot identify lost acknowledgements or safely restore after a crash. The alternative of shipping only private views leaves hybrid, profile, tool and physical-placement writes unsolved. Preserve external edits and block unrecognized state rather than guessing. See [the recovery protocol](../architecture.md#persistence-and-recovery) and its platform gate.
