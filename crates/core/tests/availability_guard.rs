//! Guards the availability record against the runtime evidence document.
//!
//! `docs/research/m1-native-results.md` and [`PROOFS`] describe the same runs.
//! When they drift, one of them is a false availability claim, which is how two
//! gates were opened in the past without a run. This test parses the document's
//! evidence matrices and fails on any disagreement:
//!
//! - a proof in Rust whose witness row is `not run` or `N/A`,
//! - a row that says `ran ... passed` for a witness the record names while the
//!   record has no proof for that configuration,
//! - a proof that names a witness no test defines,
//! - a proof whose revision the document does not mention.
//!
//! A witness the record does not name is not governed by the record; the
//! document may still record an unrelated observation of it.
//!
//! The parser is strict: an unrecognised evidence matrix cell, row or column
//! fails the test instead of being skipped.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use marduk_core::qualification::{
    Capability, Os, PROOFS, Proof, Route, THEORETICAL, VolumeClass, available, missing_witness,
};

const EVIDENCE: &str = "docs/research/m1-native-results.md";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Status {
    Ran,
    Unrun,
    NotApplicable,
    Theoretical,
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
    if lowered == "theoretically approved for testing, empirical proof deferred" {
        return Status::Theoretical;
    }
    if matches!(cell, "N/A" | "—" | "-" | "") {
        return Status::NotApplicable;
    }
    panic!("{EVIDENCE}:{line}: unrecognised evidence cell {cell:?} for `{witness}`");
}

fn observed_cells(text: &str) -> BTreeMap<(String, Os, VolumeClass), Status> {
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
                let status = parse_status(*line, &witness, &row[*column]);
                let previous = observed.insert((witness.clone(), *os, *volume), status);
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
fn primitive_cells(text: &str) -> BTreeMap<(String, Os, VolumeClass), Status> {
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
            let status = parse_status(index + 1, &witnesses.join(","), &fields[ntfs]);
            for witness in witnesses {
                let previous =
                    observed.insert((witness.clone(), Os::Windows, VolumeClass::Ntfs), status);
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
fn staging_cells(text: &str) -> BTreeMap<(String, Os, VolumeClass), (usize, Status)> {
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
            let status = parse_status(*line, &witness, &row[*column]);
            let previous = observed.insert((witness.clone(), *os, *volume), (*line, status));
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
        assert!(
            text.contains(proof.revision),
            "availability record claims {} {} on {}/{} at revision `{}`, which {EVIDENCE} \
             does not mention",
            proof.layer,
            proof.capability,
            proof.os,
            proof.volume,
            proof.revision
        );
    }

    let mut observed = observed_cells(&text);
    for (key, status) in primitive_cells(&text) {
        let previous = observed.insert(key, status);
        assert!(previous.is_none(), "{EVIDENCE}: a witness appears twice");
    }
    assert!(
        !observed.is_empty(),
        "{EVIDENCE}: no evidence matrices parsed"
    );
    for ((witness, os, volume), status) in &observed {
        let proofs = proofs_naming(witness);
        if proofs.is_empty() {
            // The record does not govern this witness; the row stands alone.
            continue;
        }
        let recorded = PROOFS.iter().any(|proof| {
            proof.os == *os
                && proof.volume == *volume
                && proof.witnesses.contains(&witness.as_str())
        });
        match status {
            Status::Ran => assert!(
                recorded,
                "{EVIDENCE} records `{witness}` as ran and passed on {os} {volume}, but the \
                 availability record has no proof for it; add the witness to the record or \
                 correct the row"
            ),
            Status::Unrun | Status::NotApplicable | Status::Theoretical => assert!(
                !recorded,
                "the availability record proves `{witness}` on {os} {volume}, but {EVIDENCE} \
                 does not record a pass for it; run the test or withdraw the proof"
            ),
        }
    }
}

#[test]
fn theoretical_candidates_match_the_document_without_qualifying_a_route() {
    let text = evidence_text();
    let documented: BTreeSet<_> = observed_cells(&text)
        .into_iter()
        .chain(primitive_cells(&text))
        .filter_map(|(key, status)| (status == Status::Theoretical).then_some(key))
        .collect();
    let recorded: BTreeSet<_> = THEORETICAL
        .iter()
        .map(|candidate| (candidate.witness.to_owned(), candidate.os, candidate.volume))
        .collect();
    assert_eq!(documented, recorded);
    for candidate in THEORETICAL {
        for route in [Route::Adapter, Route::Import, Route::Deploy, Route::Restore] {
            assert!(
                !available(candidate.capability, route, candidate.os, candidate.volume),
                "{candidate:?} is theoretical, but {route:?} is available"
            );
        }
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
            let (line, status) = cells
                .get(&(witness.to_string(), proof.os, proof.volume))
                .unwrap_or_else(|| {
                    panic!(
                        "{EVIDENCE}: the staging matrix has no `{witness}` cell for {} {}",
                        proof.os, proof.volume
                    )
                });
            assert!(
                *status != Status::Unrun,
                "{EVIDENCE}:{line}: `{witness}` is `not run` on {} {}, but the availability \
                 record proves {} there; run the test or withdraw the proof",
                proof.os,
                proof.volume,
                proof.capability
            );
        }
    }

    for ((witness, os, volume), (line, status)) in &cells {
        if *status != Status::Ran {
            continue;
        }
        let recorded = proofs
            .iter()
            .any(|proof| proof.os == *os && proof.volume == *volume);
        assert!(
            recorded,
            "{EVIDENCE}:{line}: the staging matrix records `{witness}` as ran and passed on \
             {os} {volume}, but the availability record has no {} proof for that configuration; \
             add the proof or correct the row",
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
