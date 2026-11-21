-- M1 operation journal: durable plans, root leases and deployment/restore
-- generations. Evidence blobs are versioned portable encodings validated on
-- decode; enums use snake_case literals and are checked again in Rust.

CREATE TABLE installations (
    id INTEGER PRIMARY KEY AUTOINCREMENT CHECK (id > 0),
    selected INTEGER NOT NULL CHECK (selected IN (0, 1)),
    location BLOB NOT NULL,
    installation BLOB NOT NULL,
    data BLOB NOT NULL,
    UNIQUE (installation, data)
) STRICT;

CREATE UNIQUE INDEX installations_selected
    ON installations (selected) WHERE selected = 1;

CREATE TABLE deployment_plans (
    id INTEGER PRIMARY KEY AUTOINCREMENT CHECK (id > 0),
    installation_id INTEGER NOT NULL REFERENCES installations (id),
    profile_id INTEGER NOT NULL REFERENCES profiles (id),
    revision INTEGER NOT NULL CHECK (revision >= 0),
    installation BLOB NOT NULL,
    data BLOB NOT NULL
) STRICT;

CREATE TABLE plan_intents (
    plan_id INTEGER NOT NULL REFERENCES deployment_plans (id),
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    role TEXT NOT NULL CHECK (role = 'data'),
    path TEXT NOT NULL,
    parent BLOB NOT NULL,
    leaf TEXT NOT NULL,
    source_id INTEGER NOT NULL CHECK (source_id > 0),
    source_path TEXT NOT NULL,
    source_sha BLOB NOT NULL CHECK (length(source_sha) = 32),
    source_len INTEGER NOT NULL CHECK (source_len >= 0),
    original BLOB NOT NULL,
    PRIMARY KEY (plan_id, ordinal),
    UNIQUE (plan_id, role, path),
    FOREIGN KEY (source_id, role, source_path)
        REFERENCES revision_files (revision_id, role, path)
) STRICT;

CREATE UNIQUE INDEX plan_intents_folded_path
    ON plan_intents (plan_id, role, lower(path));

CREATE TABLE operations (
    id INTEGER PRIMARY KEY AUTOINCREMENT CHECK (id > 0),
    plan_id INTEGER UNIQUE REFERENCES deployment_plans (id),
    installation_id INTEGER NOT NULL REFERENCES installations (id),
    installation BLOB NOT NULL,
    data BLOB NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('deploy', 'restore')),
    phase TEXT NOT NULL CHECK (
        phase IN ('preparing', 'prepared', 'applying', 'rolling_back',
                  'restoring', 'completed', 'rolled_back', 'cancelled')
    ),
    from_generation INTEGER REFERENCES generations (id),
    result_generation INTEGER REFERENCES generations (id),
    blocker TEXT CHECK (
        blocker IS NULL OR blocker IN (
            'stale_plan', 'active_deployment', 'busy', 'target_unavailable',
            'unsupported_target', 'invalid_path', 'recovery_conflict', 'io',
            'unknown_operation', 'no_active_deployment'
        )
    ),
    CHECK (
        (kind = 'deploy' AND plan_id IS NOT NULL AND from_generation IS NULL)
        OR (kind = 'restore' AND plan_id IS NULL AND from_generation IS NOT NULL)
    ),
    CHECK (kind = 'deploy' OR result_generation IS NULL)
) STRICT;

CREATE TABLE generations (
    id INTEGER PRIMARY KEY AUTOINCREMENT CHECK (id > 0),
    deploy_op INTEGER NOT NULL UNIQUE REFERENCES operations (id),
    data BLOB NOT NULL,
    installation BLOB NOT NULL
) STRICT;

CREATE TABLE operation_steps (
    operation_id INTEGER NOT NULL REFERENCES operations (id),
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    target BLOB NOT NULL,
    output BLOB NOT NULL,
    original_observation BLOB NOT NULL,
    original_backup BLOB,
    step_pre_observation BLOB NOT NULL,
    step_pre_backup BLOB,
    stage_ref BLOB,
    stage_fingerprint BLOB,
    displaced_ref BLOB NOT NULL,
    phase TEXT NOT NULL CHECK (
        phase IN ('ready', 'attempting', 'applied', 'undo_attempting', 'undone')
    ),
    observed BLOB,
    undo BLOB,
    PRIMARY KEY (operation_id, ordinal),
    CHECK ((original_backup IS NULL) = (substr(original_observation, 1, 1) = x'00')),
    CHECK ((step_pre_backup IS NULL) = (substr(step_pre_observation, 1, 1) = x'00')),
    CHECK ((stage_ref IS NULL) = (stage_fingerprint IS NULL))
) STRICT;

CREATE TABLE root_leases (
    data BLOB PRIMARY KEY,
    installation BLOB NOT NULL,
    pending_op INTEGER UNIQUE REFERENCES operations (id),
    active_generation INTEGER UNIQUE REFERENCES generations (id)
) STRICT;
