-- M1 library catalog: profiles, immutable revisions, staged imports.
-- IDs and revisions are nonnegative SQLite integers and are checked again on
-- every read; paths are validated ASCII relative paths and are stored as TEXT.

CREATE TABLE profiles (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    revision INTEGER NOT NULL CHECK (revision >= 0)
) STRICT;

CREATE TABLE imports (
    id INTEGER PRIMARY KEY AUTOINCREMENT CHECK (id > 0),
    profile_id INTEGER NOT NULL REFERENCES profiles (id),
    expected_revision INTEGER NOT NULL CHECK (expected_revision >= 0),
    target TEXT NOT NULL CHECK (target IN ('append', 'replace')),
    display_name TEXT,
    member_id INTEGER CHECK (member_id IS NULL OR member_id > 0),
    state TEXT NOT NULL CHECK (state IN ('writing', 'prepared', 'committed', 'discarded')),
    owner BLOB,
    archive_sha BLOB CHECK (archive_sha IS NULL OR length(archive_sha) = 32),
    result_revision INTEGER CHECK (result_revision IS NULL OR result_revision >= 0),
    CHECK (
        (target = 'append' AND display_name IS NOT NULL AND member_id IS NULL)
        OR (target = 'replace' AND member_id IS NOT NULL AND display_name IS NULL)
    ),
    CHECK (
        state NOT IN ('prepared', 'committed')
        OR (owner IS NOT NULL AND archive_sha IS NOT NULL)
    ),
    CHECK ((state = 'committed') = (result_revision IS NOT NULL))
) STRICT;

CREATE TABLE installed_revisions (
    id INTEGER PRIMARY KEY REFERENCES imports (id) CHECK (id > 0)
) STRICT;

CREATE TABLE revision_files (
    revision_id INTEGER NOT NULL REFERENCES installed_revisions (id),
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    role TEXT NOT NULL CHECK (role = 'data'),
    path TEXT NOT NULL,
    len INTEGER NOT NULL CHECK (len >= 0),
    sha BLOB NOT NULL CHECK (length(sha) = 32),
    PRIMARY KEY (revision_id, role, path),
    UNIQUE (revision_id, ordinal)
) STRICT;

CREATE UNIQUE INDEX revision_files_folded_path
    ON revision_files (revision_id, role, lower(path));

CREATE TABLE members (
    id INTEGER PRIMARY KEY AUTOINCREMENT CHECK (id > 0),
    profile_id INTEGER NOT NULL REFERENCES profiles (id),
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    installed_id INTEGER NOT NULL REFERENCES installed_revisions (id),
    enabled INTEGER NOT NULL CHECK (enabled IN (0, 1)),
    display_name TEXT NOT NULL,
    UNIQUE (profile_id, ordinal)
) STRICT;

CREATE TABLE staged_files (
    import_id INTEGER NOT NULL REFERENCES imports (id),
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    role TEXT NOT NULL CHECK (role = 'data'),
    path TEXT NOT NULL,
    len INTEGER NOT NULL CHECK (len >= 0),
    sha BLOB NOT NULL CHECK (length(sha) = 32),
    PRIMARY KEY (import_id, role, path),
    UNIQUE (import_id, ordinal)
) STRICT;

CREATE UNIQUE INDEX staged_files_folded_path
    ON staged_files (import_id, role, lower(path));

CREATE TABLE import_result_members (
    import_id INTEGER NOT NULL REFERENCES imports (id),
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    member_id INTEGER NOT NULL CHECK (member_id > 0),
    installed_id INTEGER NOT NULL REFERENCES installed_revisions (id),
    enabled INTEGER NOT NULL CHECK (enabled IN (0, 1)),
    display_name TEXT NOT NULL,
    PRIMARY KEY (import_id, ordinal),
    UNIQUE (import_id, member_id)
) STRICT;
