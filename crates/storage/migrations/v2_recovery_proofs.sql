-- Additive v2 recovery records. Never reconstruct missing proofs from the
-- mutable rows they protect: catalogs without a proof fail closed.
CREATE TABLE IF NOT EXISTS plan_proofs (
    plan_id INTEGER PRIMARY KEY REFERENCES deployment_plans (id),
    intents_sha BLOB NOT NULL CHECK (length(intents_sha) = 32)
) STRICT;

CREATE TABLE IF NOT EXISTS operation_proofs (
    operation_id INTEGER PRIMARY KEY REFERENCES operations (id),
    location BLOB NOT NULL,
    intents_sha BLOB CHECK (length(intents_sha) = 32)
) STRICT;

CREATE TABLE IF NOT EXISTS unexpected_displacements (
    operation_id INTEGER PRIMARY KEY REFERENCES operations (id),
    ordinal INTEGER NOT NULL,
    direction INTEGER NOT NULL CHECK (direction IN (0, 1)),
    reported_ref BLOB NOT NULL,
    evidence BLOB NOT NULL,
    FOREIGN KEY (operation_id, ordinal) REFERENCES operation_steps (operation_id, ordinal)
) STRICT;

CREATE TABLE IF NOT EXISTS displacement_evidence (
    operation_id INTEGER NOT NULL,
    ordinal INTEGER NOT NULL,
    direction INTEGER NOT NULL CHECK (direction IN (0, 1)),
    reported_ref BLOB NOT NULL,
    evidence BLOB NOT NULL,
    PRIMARY KEY (operation_id, ordinal, direction, reported_ref),
    FOREIGN KEY (operation_id, ordinal) REFERENCES operation_steps (operation_id, ordinal)
) STRICT;

INSERT OR IGNORE INTO displacement_evidence SELECT * FROM unexpected_displacements;
DELETE FROM unexpected_displacements;

-- Append-only slot history. Each row is one inspection of one exact sealed
-- slot: the canonical retain or its deterministic alternate, forward and undo
-- directions distinct. `slot_state` is 0 for a readable expected slot, 1 for a
-- reported displacement and 2 for an unreadable slot; the greatest `seq` per
-- slot decides what status may advertise. `displacement_evidence` above is the
-- pre-history form, copied once per slot and never written again; a copied row
-- is treated as a report and is still advertised only when its stored evidence
-- recorded a positive read.
CREATE TABLE IF NOT EXISTS displacement_observations (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    operation_id INTEGER NOT NULL,
    ordinal INTEGER NOT NULL,
    direction INTEGER NOT NULL CHECK (direction IN (0, 1)),
    reported_ref BLOB NOT NULL,
    evidence BLOB NOT NULL,
    slot_state INTEGER NOT NULL CHECK (slot_state IN (0, 1, 2)),
    FOREIGN KEY (operation_id, ordinal) REFERENCES operation_steps (operation_id, ordinal)
) STRICT;

CREATE INDEX IF NOT EXISTS displacement_observations_slot
    ON displacement_observations (operation_id, ordinal, direction, reported_ref, seq);

INSERT INTO displacement_observations
    (operation_id, ordinal, direction, reported_ref, evidence, slot_state)
SELECT legacy.operation_id, legacy.ordinal, legacy.direction, legacy.reported_ref,
       legacy.evidence, 1
FROM displacement_evidence AS legacy
WHERE NOT EXISTS (
    SELECT 1 FROM displacement_observations AS kept
    WHERE kept.operation_id = legacy.operation_id
      AND kept.ordinal = legacy.ordinal
      AND kept.direction = legacy.direction
      AND kept.reported_ref = legacy.reported_ref
);
