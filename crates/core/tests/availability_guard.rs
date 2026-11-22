//! Guards the availability record against the runtime evidence document.
//!
//! `docs/research/m1-native-results.md` and [`PROOFS`] describe the same runs.
//! When they drift, one of them is a false availability claim, which is how two
//! gates were opened in the past without a run. This test parses the document's
//! evidence matrices and fails on any disagreement:
//!
//! - a proof in Rust whose matching witness/configuration cell is missing,
//!   not run, N/A, or names a different revision than the proof,
//! - a row that says `ran ... passed` for a witness the record names while the
//!   record has no proof for that configuration at that revision,
//! - a proof that names a witness no test defines.
//!
//! A witness the record does not name is not governed by the record; the
//! document may still record an unrelated observation of it.
//!
//! The parser is strict: an unrecognised evidence matrix cell, row or column
//! fails the test instead of being skipped.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use marduk_core::qualification::{
    Capability, Layer, NARROWED, Os, PROOFS, Proof, Route, VolumeClass, available, missing_witness,
};

const EVIDENCE: &str = "docs/research/m1-native-results.md";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Status {
    Ran,
    Unrun,
    NotApplicable,
    /// A recorded decision that narrows the guarantee. It is deliberately not
    /// a runtime pass, so it can never satisfy a proof or imply crash coverage.
    Narrowed,
}

/// One evidence cell: where it is, what it says, and the revisions it names.
///
/// A pass satisfies a proof only when the cell names the proof's revision; a
/// revision in unrelated prose is not evidence for this configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Cell {
    line: usize,
    status: Status,
    revisions: Vec<String>,
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn evidence_text() -> String {
    let path = repo_root().join(EVIDENCE);
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// Every `fn name` in a Rust source file under `crates/` or `tests/`.
fn collect_defined_names(directory: &Path, names: &mut BTreeSet<String>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        if name == "target" || name == ".git" {
            continue;
        }
        if path.is_dir() {
            collect_defined_names(&path, names);
            continue;
        }
        if path.extension().is_none_or(|extension| extension != "rs") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line in text.lines() {
            let mut rest = line.trim_start();
            while let Some(tail) = rest
                .strip_prefix("pub(crate) ")
                .or_else(|| rest.strip_prefix("pub "))
                .or_else(|| rest.strip_prefix("async "))
                .or_else(|| rest.strip_prefix("const "))
                .or_else(|| rest.strip_prefix("unsafe "))
            {
                rest = tail.trim_start();
            }
            let Some(rest) = rest.strip_prefix("fn ") else {
                continue;
            };
            let ident: String = rest
                .chars()
                .take_while(|character| character.is_alphanumeric() || *character == '_')
                .collect();
            if !ident.is_empty() {
                names.insert(ident);
            }
        }
    }
}

/// A parsed evidence matrix: header columns mapped to configurations.
struct Matrix {
    first_header: String,
    columns: Vec<(usize, Option<(Os, VolumeClass)>)>,
    rows: Vec<(usize, Vec<String>)>,
}

enum Column {
    Configuration(Os, VolumeClass),
    Ignored,
    Unknown,
}

fn classify_column(header: &str) -> Column {
    let header = header.trim();
    let lowered = header.to_ascii_lowercase();
    if lowered.contains("linux btrfs") {
        return Column::Configuration(Os::Linux, VolumeClass::Btrfs);
    }
    if lowered.contains("linux tmpfs") {
        return Column::Configuration(Os::Linux, VolumeClass::Tmpfs);
    }
    if lowered.contains("linux ext4") {
        return Column::Configuration(Os::Linux, VolumeClass::Ext4);
    }
    if lowered.contains("ntfs") {
        return Column::Configuration(Os::Windows, VolumeClass::Ntfs);
    }
    if lowered == "power" {
        return Column::Ignored;
    }
    Column::Unknown
}

fn cells(line: &str) -> Vec<String> {
    line.trim()
        .trim_matches('|')
        .split('|')
        .map(|cell| cell.trim().to_string())
        .collect()
}

fn is_separator(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with('|')
        && trimmed
            .chars()
            .all(|character| matches!(character, '|' | '-' | ':' | ' '))
}

/// Split the document into tables: a header line, a separator line, then rows.
fn matrices(text: &str) -> Vec<Matrix> {
    let lines: Vec<&str> = text.lines().collect();
    let mut matrices = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let header = lines[index];
        let next = lines.get(index + 1).copied().unwrap_or_default();
        if !header.trim_start().starts_with('|') || !is_separator(next) {
            index += 1;
            continue;
        }
        let header_cells = cells(header);
        let classified: Vec<Column> = header_cells
            .iter()
            .map(|header| classify_column(header))
            .collect();
        let filesystem_columns = classified
            .iter()
            .enumerate()
            .filter(|(column, config)| *column > 0 && matches!(config, Column::Configuration(..)))
            .count();
        let table_line = index + 1;
        index += 2;
        let mut rows = Vec::new();
        while let Some(row) = lines.get(index) {
            if !row.trim_start().starts_with('|') {
                break;
            }
            rows.push((index + 1, cells(row)));
            index += 1;
        }
        if filesystem_columns < 2 {
            // Not an evidence matrix (a suite or primitive table). Leave it.
            continue;
        }
        for (column, config) in classified.iter().enumerate() {
            if column == 0 {
                continue;
            }
            assert!(
                !matches!(config, Column::Unknown),
                "{EVIDENCE}:{table_line}: unrecognised evidence column {:?}; expected a \
                 Linux Btrfs, Linux tmpfs, Linux ext4, NTFS or Power column",
                header_cells[column]
            );
        }
        let columns = classified
            .into_iter()
            .enumerate()
            .map(|(column, config)| {
                let configuration = match config {
                    Column::Configuration(os, volume) => Some((os, volume)),
                    Column::Ignored | Column::Unknown => None,
                };
                (column, configuration)
            })
            .collect();
        matrices.push(Matrix {
            first_header: header_cells.first().cloned().unwrap_or_default(),
            columns,
            rows,
        });
    }
    matrices
}

/// The backticked witness name at the start of a row's first cell.
fn witness_name(cell: &str) -> Option<&str> {
    let cell = cell.trim();
    let rest = cell.strip_prefix('`')?;
    let end = rest.find('`')?;
    Some(rest[..end].trim())
}

fn strip_note(cell: &str) -> &str {
    let cell = cell.trim();
    match cell.find(" (") {
        Some(position) => cell[..position].trim(),
        None => cell,
    }
}

fn parse_status(line: usize, witness: &str, cell: &str) -> Status {
    let cell = strip_note(cell);
    let lowered = cell.to_ascii_lowercase();
    if lowered.starts_with("ran") {
        assert!(
            lowered.contains("passed"),
            "{EVIDENCE}:{line}: `{witness}` cell says {cell:?} without a pass"
        );
        return Status::Ran;
    }
    if lowered.starts_with("not run") {
        return Status::Unrun;
    }
    if lowered.starts_with("narrowed") {
        return Status::Narrowed;
    }
    if matches!(cell, "N/A" | "—" | "-" | "") {
        return Status::NotApplicable;
    }
    panic!("{EVIDENCE}:{line}: unrecognised evidence cell {cell:?} for `{witness}`");
}

/// Revisions named in a cell. Evidence writes them as a hex token, usually
/// backticked (`eb39be0`), but some older rows omit the backticks, so the
/// token shape is the contract, not the formatting.
fn revisions(cell: &str) -> Vec<String> {
    cell.split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|token| {
            token.len() >= 7
                && token.len() <= 12
                && token.chars().all(|character| character.is_ascii_hexdigit())
                && token.chars().any(|character| character.is_ascii_digit())
        })
        .map(str::to_owned)
        .collect()
}

fn parse_cell(line: usize, witness: &str, cell: &str) -> Cell {
    let status = parse_status(line, witness, cell);
    Cell {
        line,
        status,
        revisions: revisions(strip_note(cell)),
    }
}

fn observed_cells(text: &str) -> BTreeMap<(String, Os, VolumeClass), Cell> {
    let mut observed = BTreeMap::new();
    for matrix in matrices(text) {
        for (line, row) in &matrix.rows {
            assert_eq!(
                row.len(),
                matrix.columns.len(),
                "{EVIDENCE}:{line}: row has {} cells, header has {}",
                row.len(),
                matrix.columns.len()
            );
            let witness = witness_name(&row[0]).unwrap_or_else(|| {
                panic!(
                    "{EVIDENCE}:{line}: first cell must be one backticked witness name, found {:?}",
                    row[0]
                )
            });
            assert!(
                !witness.is_empty() && !witness.contains(' '),
                "{EVIDENCE}:{line}: first cell must be one backticked witness name, found {:?}",
                row[0]
            );
            let witness = witness.to_string();
            for (column, config) in &matrix.columns {
                let Some((os, volume)) = config else {
                    continue;
                };
                let cell = parse_cell(*line, &witness, &row[*column]);
                let previous = observed.insert((witness.clone(), *os, *volume), cell);
                assert!(
                    previous.is_none(),
                    "{EVIDENCE}:{line}: `{witness}` appears twice for {os} {volume}"
                );
            }
        }
    }
    observed
}

/// Witnesses named in a runtime cell, with a module prefix removed.
fn backticked_names(cell: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = cell;
    while let Some(start) = rest.find('`') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('`') else {
            break;
        };
        let name = &after[..end];
        let leaf = name.rsplit("::").next().unwrap_or(name).trim();
        if !leaf.is_empty() && !leaf.contains(' ') {
            names.push(leaf.to_string());
        }
        rest = &after[end + 1..];
    }
    names
}

/// Primitive tables list a test in the Linux runtime column and one status for
/// the Windows NTFS column, so that cell is the Windows status of every test the
/// row names. Only tables that are not evidence matrices are read here.
fn primitive_cells(text: &str) -> BTreeMap<(String, Os, VolumeClass), Cell> {
    let lines: Vec<&str> = text.lines().collect();
    let mut observed = BTreeMap::new();
    let mut index = 0;
    while index < lines.len() {
        let header = lines[index];
        let separator = lines.get(index + 1).copied().unwrap_or_default();
        if !header.trim_start().starts_with('|') || !is_separator(separator) {
            index += 1;
            continue;
        }
        let headers = cells(header);
        let runtime = headers
            .iter()
            .position(|cell| cell.to_ascii_lowercase().contains("linux runtime"));
        let ntfs = headers
            .iter()
            .position(|cell| cell.to_ascii_lowercase().contains("ntfs"));
        index += 2;
        while let Some(row) = lines.get(index) {
            if !row.trim_start().starts_with('|') {
                break;
            }
            let (Some(runtime), Some(ntfs)) = (runtime, ntfs) else {
                index += 1;
                continue;
            };
            let fields = cells(row);
            assert_eq!(
                fields.len(),
                headers.len(),
                "{EVIDENCE}:{}: row has {} cells, header has {}",
                index + 1,
                fields.len(),
                headers.len()
            );
            let witnesses = backticked_names(&fields[runtime]);
            let cell = parse_cell(index + 1, &witnesses.join(","), &fields[ntfs]);
            for witness in witnesses {
                let previous = observed.insert(
                    (witness.clone(), Os::Windows, VolumeClass::Ntfs),
                    cell.clone(),
                );
                assert!(
                    previous.is_none(),
                    "{EVIDENCE}:{}: `{witness}` appears twice for Windows NTFS",
                    index + 1
                );
            }
            index += 1;
        }
    }
    observed
}

/// The `LibraryStaging` witness matrix. It is the only matrix whose first
/// header is the staging-witness column, and it must carry a column for every
/// configuration a staging proof names today.
fn staging_cells(text: &str) -> BTreeMap<(String, Os, VolumeClass), Cell> {
    let mut selected: Vec<Matrix> = matrices(text)
        .into_iter()
        .filter(|matrix| matrix.first_header.eq_ignore_ascii_case("Staging witness"))
        .collect();
    assert_eq!(
        selected.len(),
        1,
        "{EVIDENCE}: expected exactly one staging witness matrix, found {}",
        selected.len()
    );
    let matrix = selected.pop().expect("one selected matrix");
    for required in [
        (Os::Linux, VolumeClass::Btrfs),
        (Os::Linux, VolumeClass::Tmpfs),
        (Os::Windows, VolumeClass::Ntfs),
    ] {
        assert!(
            matrix
                .columns
                .iter()
                .any(|(_, config)| *config == Some(required)),
            "{EVIDENCE}: the staging matrix has no {} {} column",
            required.0,
            required.1
        );
    }
    let mut observed = BTreeMap::new();
    for (line, row) in &matrix.rows {
        assert_eq!(
            row.len(),
            matrix.columns.len(),
            "{EVIDENCE}:{line}: row has {} cells, header has {}",
            row.len(),
            matrix.columns.len()
        );
        let witness = witness_name(&row[0]).unwrap_or_else(|| {
            panic!(
                "{EVIDENCE}:{line}: first cell must be one backticked witness name, found {:?}",
                row[0]
            )
        });
        assert!(
            !witness.is_empty() && !witness.contains(' '),
            "{EVIDENCE}:{line}: first cell must be one backticked witness name, found {:?}",
            row[0]
        );
        let witness = witness.to_string();
        for (column, config) in &matrix.columns {
            let Some((os, volume)) = config else {
                continue;
            };
            let status = parse_cell(*line, &witness, &row[*column]);
            let previous = observed.insert((witness.clone(), *os, *volume), status);
            assert!(
                previous.is_none(),
                "{EVIDENCE}:{line}: `{witness}` appears twice in the staging matrix for {os} {volume}"
            );
        }
    }
    observed
}

fn proofs_naming(witness: &str) -> Vec<&'static Proof> {
    PROOFS
        .iter()
        .filter(|proof| proof.witnesses.contains(&witness))
        .collect()
}

#[test]
fn the_record_and_the_evidence_document_agree() {
    let text = evidence_text();
    let mut defined = BTreeSet::new();
    let root = repo_root();
    collect_defined_names(&root.join("crates"), &mut defined);
    collect_defined_names(&root.join("tests"), &mut defined);
    let lookup = |name: &str| defined.contains(name);

    for proof in PROOFS {
        if let Some(witness) = missing_witness(proof, &lookup) {
            panic!(
                "availability record claims {} {} on {}/{} but names witness `{witness}`, \
                 which no test defines",
                proof.layer, proof.capability, proof.os, proof.volume
            );
        }
    }

    let mut observed = observed_cells(&text);
    for (key, cell) in primitive_cells(&text) {
        let previous = observed.insert(key, cell);
        assert!(previous.is_none(), "{EVIDENCE}: a witness appears twice");
    }
    assert!(
        !observed.is_empty(),
        "{EVIDENCE}: no evidence matrices parsed"
    );

    // Record -> document: each named witness needs its own passing cell on the
    // proof's configuration at the proof's revision. A revision in unrelated
    // prose is not evidence for this row.
    for proof in PROOFS {
        for witness in proof.witnesses {
            let cell = observed
                .get(&(witness.to_string(), proof.os, proof.volume))
                .unwrap_or_else(|| {
                    panic!(
                        "availability record claims {} {} on {}/{} but {EVIDENCE} has no cell \
                         for `{witness}` there; add the row or withdraw the proof",
                        proof.layer, proof.capability, proof.os, proof.volume
                    )
                });
            assert!(
                cell.status == Status::Ran,
                "availability record claims {} {} on {}/{} but {EVIDENCE}:{} records \
                 `{witness}` as {:?}, not a pass",
                proof.layer,
                proof.capability,
                proof.os,
                proof.volume,
                cell.line,
                cell.status
            );
            assert!(
                cell.revisions
                    .iter()
                    .any(|revision| revision == proof.revision),
                "availability record claims {} {} on {}/{} at revision `{}`, but {EVIDENCE}:{} \
                 records `{witness}` at {:?}",
                proof.layer,
                proof.capability,
                proof.os,
                proof.volume,
                proof.revision,
                cell.line,
                cell.revisions
            );
        }
    }

    // Document -> record: a pass is exact. Every revision the cell names must
    // be proved for that witness and configuration, and a non-pass may not
    // contradict a proof.
    for ((witness, os, volume), cell) in &observed {
        let proofs = proofs_naming(witness);
        if proofs.is_empty() {
            // The record does not govern this witness; the row stands alone.
            continue;
        }
        let here: Vec<&Proof> = proofs
            .iter()
            .copied()
            .filter(|proof| proof.os == *os && proof.volume == *volume)
            .collect();
        match cell.status {
            Status::Ran => {
                assert!(
                    !here.is_empty(),
                    "{EVIDENCE}:{} records `{witness}` as ran and passed on {os} {volume}, but \
                     the availability record has no proof for it; add the witness to the record \
                     or correct the row",
                    cell.line
                );
                assert!(
                    !cell.revisions.is_empty(),
                    "{EVIDENCE}:{} records `{witness}` as ran and passed on {os} {volume} \
                     without naming a revision",
                    cell.line
                );
                for revision in &cell.revisions {
                    assert!(
                        here.iter().any(|proof| proof.revision == revision),
                        "{EVIDENCE}:{} records `{witness}` as passed on {os} {volume} at \
                         revision `{revision}`, which the availability record does not prove; \
                         add the proof or correct the row",
                        cell.line
                    );
                }
            }
            Status::Unrun | Status::NotApplicable | Status::Narrowed => assert!(
                here.is_empty(),
                "the availability record proves `{witness}` on {os} {volume}, but {EVIDENCE}:{} \
                 does not record a pass for it; run the test or withdraw the proof",
                cell.line
            ),
        }
    }
}

#[test]
fn a_composition_proof_opens_deploy_without_opening_an_unproven_configuration() {
    for proof in PROOFS
        .iter()
        .filter(|proof| proof.layer == Layer::Composition)
    {
        for route in [Route::Deploy, Route::Restore] {
            assert!(
                available(proof.capability, route, proof.os, proof.volume),
                "{} {route:?} on {}/{}",
                proof.capability,
                proof.os,
                proof.volume
            );
        }
    }
    // Windows deploy is opened by the NTFS composition run, not by the
    // narrowed barrier row (ADR 0004).
    assert!(available(
        Capability::NativeEffects,
        Route::Deploy,
        Os::Windows,
        VolumeClass::Ntfs
    ));
    assert!(available(
        Capability::NativeEffects,
        Route::Restore,
        Os::Windows,
        VolumeClass::Ntfs
    ));
    assert!(!available(
        Capability::NativeEffects,
        Route::Deploy,
        Os::Linux,
        VolumeClass::Ext4
    ));
    assert!(!available(
        Capability::NativeEffects,
        Route::Deploy,
        Os::Windows,
        VolumeClass::Other
    ));
}

/// A `narrowed` cell records an ADR decision, not a run. It may accompany a
/// configuration whose composed deploy is separately proven, but it must never
/// be the witness that proves anything, and it must never be read as a pass for
/// the power-cut experiment that no run covers.
#[test]
fn narrowed_rows_match_the_document_and_never_stand_in_for_a_run() {
    let text = evidence_text();
    let documented: BTreeSet<_> = observed_cells(&text)
        .into_iter()
        .chain(primitive_cells(&text))
        .filter_map(|(key, cell)| (cell.status == Status::Narrowed).then_some(key))
        .collect();
    let recorded: BTreeSet<_> = NARROWED
        .iter()
        .map(|candidate| (candidate.witness.to_owned(), candidate.os, candidate.volume))
        .collect();
    assert_eq!(documented, recorded);

    for candidate in NARROWED {
        assert!(!candidate.adr.is_empty(), "{candidate:?} names no decision");
        for proof in proofs_naming(candidate.witness) {
            assert!(
                proof.os != candidate.os || proof.volume != candidate.volume,
                "`{}` is narrowed on {}/{} and cannot prove {proof:?}",
                candidate.witness,
                candidate.os,
                candidate.volume
            );
        }
        // The weakened barrier admits the composed route only because the
        // composition itself ran. Without that proof the route stays closed.
        let proved = PROOFS.iter().any(|proof| {
            proof.capability == candidate.capability
                && proof.os == candidate.os
                && proof.volume == candidate.volume
                && proof.layer >= Layer::Composition
        });
        assert_eq!(
            available(
                candidate.capability,
                Route::Deploy,
                candidate.os,
                candidate.volume
            ),
            proved,
            "a narrowed row must not open deploy on {}/{} on its own",
            candidate.os,
            candidate.volume
        );
    }
}

#[test]
fn staging_evidence_matches_the_library_staging_proofs() {
    let text = evidence_text();
    let cells = staging_cells(&text);
    let proofs: Vec<&Proof> = PROOFS
        .iter()
        .filter(|proof| proof.capability == Capability::LibraryStaging)
        .collect();

    for proof in &proofs {
        for witness in proof.witnesses {
            assert!(
                cells.keys().any(|(name, _, _)| name == witness),
                "{EVIDENCE} has no staging matrix row for `{witness}`, which the availability \
                 record proves for {} on {} {}; add the row or withdraw the proof",
                proof.capability,
                proof.os,
                proof.volume
            );
        }
    }

    for proof in &proofs {
        for witness in proof.witnesses {
            let cell = cells
                .get(&(witness.to_string(), proof.os, proof.volume))
                .unwrap_or_else(|| {
                    panic!(
                        "{EVIDENCE}: the staging matrix has no `{witness}` cell for {} {}",
                        proof.os, proof.volume
                    )
                });
            assert!(
                cell.status == Status::Ran,
                "{EVIDENCE}:{}: `{witness}` is {:?} on {} {}, but the availability record \
                 proves {} there; run the test or withdraw the proof",
                cell.line,
                cell.status,
                proof.os,
                proof.volume,
                proof.capability
            );
        }
    }

    for ((witness, os, volume), cell) in &cells {
        if cell.status != Status::Ran {
            continue;
        }
        let recorded = proofs
            .iter()
            .any(|proof| proof.os == *os && proof.volume == *volume);
        assert!(
            recorded,
            "{EVIDENCE}:{}: the staging matrix records `{witness}` as ran and passed on \
             {os} {volume}, but the availability record has no {} proof for that configuration; \
             add the proof or correct the row",
            cell.line,
            Capability::LibraryStaging
        );
    }
}

#[test]
fn every_matrix_row_names_a_test() {
    let text = evidence_text();
    let mut defined = BTreeSet::new();
    let root = repo_root();
    collect_defined_names(&root.join("crates"), &mut defined);
    collect_defined_names(&root.join("tests"), &mut defined);
    for matrix in matrices(&text) {
        for (line, row) in &matrix.rows {
            let witness = witness_name(&row[0]).unwrap_or_else(|| {
                panic!(
                    "{EVIDENCE}:{line}: first cell must be one backticked witness name, found {:?}",
                    row[0]
                )
            });
            assert!(
                defined.contains(witness),
                "{EVIDENCE}:{line}: evidence row `{witness}` names no test in the tree"
            );
        }
    }
    for (witness, _, _) in primitive_cells(&text).keys() {
        assert!(
            defined.contains(witness),
            "{EVIDENCE}: a primitive row names `{witness}`, which no test in the tree defines"
        );
    }
}
