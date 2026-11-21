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
