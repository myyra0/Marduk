use marduk_core::{
    ids::{InstalledRevisionId, MemberId, ProfileId, ProfileRevision},
    profiles::{ProfileMember, ProfileSnapshot},
    resolve::{ResolveError, ResolvedEntry, ResolvedFiles, ResolvedInputs, resolve_files},
    targets::{PathError, PathViolation, RelativePath, RootRole, TargetFile},
};

const A: u64 = 11;
const B: u64 = 7;
const A_REV: u64 = 41;
const B_REV: u64 = 29;
const WINNER: &str = "textures/winner.dds";
const A_ONLY: &str = "textures/a-only.dds";
const Z_LAST: &str = "Textures/Z-last.dds";

fn member(id: u64, installed: u64, enabled: bool) -> ProfileMember {
    ProfileMember {
        id: MemberId::new(id),
        installed: InstalledRevisionId::new(installed),
        enabled,
    }
}

fn profile(members: Vec<ProfileMember>) -> ProfileSnapshot {
    ProfileSnapshot {
        id: ProfileId::new(3),
        revision: ProfileRevision::new(13),
        members,
    }
}

fn inputs(manifests: &[(u64, &[&str])]) -> ResolvedInputs {
    ResolvedInputs {
        manifests: manifests
            .iter()
            .map(|&(installed, paths)| {
                let targets = paths
                    .iter()
                    .map(|path| TargetFile {
                        root: RootRole::Data,
                        relative: RelativePath::parse(path).expect("valid fixture path"),
                    })
                    .collect();
                (InstalledRevisionId::new(installed), targets)
            })
            .collect(),
    }
}

fn data_path(target: &TargetFile) -> &str {
    assert!(matches!(&target.root, RootRole::Data));
    target.relative.as_str()
}

fn assert_entry(entry: &ResolvedEntry, target: &str, candidates: &[(u64, u64, &str)]) {
    assert_eq!(data_path(&entry.target), target);
    let actual: Vec<_> = entry
        .candidates
        .iter()
        .map(|c| (c.member.get(), c.installed.get(), data_path(&c.input)))
        .collect();
    assert_eq!(actual.as_slice(), candidates, "candidates for {target}");
}

fn entries(snapshot: &ProfileSnapshot, resolved: ResolvedFiles) -> Vec<ResolvedEntry> {
    assert_eq!(resolved.revision.get(), snapshot.revision.get());
    resolved.entries
}

#[test]
fn later_enabled_member_wins_and_keeps_provenance() {
    let mut snapshot = profile(vec![member(A, A_REV, true), member(B, B_REV, true)]);
    let manifests = inputs(&[(A_REV, &[Z_LAST, WINNER, A_ONLY]), (B_REV, &[WINNER])]);

    let resolved = entries(&snapshot, resolve_files(&snapshot, &manifests).unwrap());
    assert_eq!(resolved.len(), 3);
    assert_entry(&resolved[0], A_ONLY, &[(A, A_REV, A_ONLY)]);
    assert_entry(
        &resolved[1],
        WINNER,
        &[(A, A_REV, WINNER), (B, B_REV, WINNER)],
    );
    assert_entry(&resolved[2], Z_LAST, &[(A, A_REV, Z_LAST)]);

    snapshot.members.reverse();
    let resolved = entries(&snapshot, resolve_files(&snapshot, &manifests).unwrap());
    assert_eq!(resolved.len(), 3);
    assert_entry(&resolved[0], A_ONLY, &[(A, A_REV, A_ONLY)]);
    assert_entry(
        &resolved[1],
        WINNER,
        &[(B, B_REV, WINNER), (A, A_REV, WINNER)],
    );
    assert_entry(&resolved[2], Z_LAST, &[(A, A_REV, Z_LAST)]);
}

#[test]
fn disabled_member_does_not_compete() {
    let mut snapshot = profile(vec![member(A, A_REV, true), member(B, B_REV, false)]);
    let missing = inputs(&[(A_REV, &[WINNER])]);
    let populated = inputs(&[(A_REV, &[WINNER]), (B_REV, &["b-only.dds"])]);
    for manifests in [&missing, &populated] {
        let resolved = entries(&snapshot, resolve_files(&snapshot, manifests).unwrap());
        assert_eq!(resolved.len(), 1);
        assert_entry(&resolved[0], WINNER, &[(A, A_REV, WINNER)]);
    }

    snapshot.members[1].enabled = true;
    let error = resolve_files(&snapshot, &missing).unwrap_err();
    assert!(
        matches!(error, ResolveError::MissingManifest(id) if id.get() == B_REV),
        "{error:?}"
    );

    let empty = inputs(&[(A_REV, &[WINNER]), (B_REV, &[])]);
    let resolved = entries(&snapshot, resolve_files(&snapshot, &empty).unwrap());
    assert_eq!(resolved.len(), 1);
    assert_entry(&resolved[0], WINNER, &[(A, A_REV, WINNER)]);
}

#[test]
fn same_mod_alias_is_invalid_but_overlap_is_allowed() {
    const CAPITALIZED: &str = "Textures/Winner.dds";
    let mut snapshot = profile(vec![member(A, A_REV, true)]);
    for second_path in [WINNER, CAPITALIZED] {
        let manifests = inputs(&[(A_REV, &[CAPITALIZED, second_path])]);
        let error = resolve_files(&snapshot, &manifests).unwrap_err();
        assert!(
            matches!(&error, ResolveError::AliasedEntries { .. }),
            "{error:?}"
        );
        let ResolveError::AliasedEntries {
            installed,
            first,
            second,
        } = error
        else {
            unreachable!();
        };
        assert_eq!(installed.get(), A_REV);
        assert_eq!(data_path(&first), CAPITALIZED);
        assert_eq!(data_path(&second), second_path);
    }

    snapshot.members.push(member(B, B_REV, true));
    let manifests = inputs(&[(A_REV, &[WINNER]), (B_REV, &[CAPITALIZED])]);
    let resolved = entries(&snapshot, resolve_files(&snapshot, &manifests).unwrap());
    assert_eq!(resolved.len(), 1);
    assert_entry(
        &resolved[0],
        CAPITALIZED,
        &[(A, A_REV, WINNER), (B, B_REV, CAPITALIZED)],
    );
}

#[test]
fn file_directory_overlap_has_no_plan() {
    const FILE: &str = "textures/pack";
    const CHILD: &str = "Textures/pack/f.dds";
    const NEIGHBOR: &str = "Textures/pack-neighbor.dds";
    const SIBLING: &str = "Textures/packet/f.dds";
    let snapshot = profile(vec![member(A, A_REV, true), member(B, B_REV, true)]);

    for (a_path, b_path) in [(FILE, CHILD), (CHILD, FILE)] {
        let manifests = inputs(&[(A_REV, &[a_path, NEIGHBOR]), (B_REV, &[b_path])]);
        let error = resolve_files(&snapshot, &manifests).unwrap_err();
        assert!(
            matches!(&error, ResolveError::ConflictingTargetShape { .. }),
            "{error:?}"
        );
        let ResolveError::ConflictingTargetShape { first, second } = error else {
            unreachable!();
        };
        let reported = (data_path(&first), data_path(&second));
        assert!(
            reported == (FILE, CHILD) || reported == (CHILD, FILE),
            "wrong conflicting paths: {reported:?}"
        );
    }

    let manifests = inputs(&[(A_REV, &[FILE]), (B_REV, &[SIBLING])]);
    let resolved = entries(&snapshot, resolve_files(&snapshot, &manifests).unwrap());
    assert_eq!(resolved.len(), 2);
    assert_entry(&resolved[0], FILE, &[(A, A_REV, FILE)]);
    assert_entry(&resolved[1], SIBLING, &[(B, B_REV, SIBLING)]);
}

#[test]
fn duplicate_member_has_no_plan() {
    let snapshot = profile(vec![member(A, A_REV, true), member(A, B_REV, false)]);
    let error = resolve_files(&snapshot, &inputs(&[])).unwrap_err();
    assert!(
        matches!(error, ResolveError::DuplicateMember(id) if id.get() == A),
        "{error:?}"
    );
}

#[test]
fn distinct_members_of_one_revision_keep_both_candidates() {
    let snapshot = profile(vec![member(A, A_REV, true), member(B, A_REV, true)]);
    let manifests = inputs(&[(A_REV, &[WINNER])]);
    let resolved = entries(&snapshot, resolve_files(&snapshot, &manifests).unwrap());
    assert_eq!(resolved.len(), 1);
    assert_entry(
        &resolved[0],
        WINNER,
        &[(A, A_REV, WINNER), (B, A_REV, WINNER)],
    );
}

#[test]
fn admitted_paths_preserve_spelling() {
    for spelling in ["Textures/Metal Plate.DDS", "Data/COM10.dds", "LPT0.txt"] {
        let path = RelativePath::parse(spelling).expect("valid portable path");
        assert_eq!(path.as_str(), spelling);
    }
}

#[test]
fn unadmitted_paths_report_first_violation() {
    use PathViolation::*;

    assert!(matches!(RelativePath::parse(""), Err(PathError::EmptyPath)));
    for (spelling, index, reason) in [
        ("/textures/x", 0, Empty),
        ("textures//x", 1, Empty),
        ("textures/", 1, Empty),
        ("textures/./x", 1, Dot),
        ("textures/../bad?", 1, Dot),
        ("C:/textures/x", 0, ForbiddenCharacter),
        ("textures\\x", 0, ForbiddenCharacter),
        ("textures/ü.dds", 1, ForbiddenCharacter),
        ("textures/\x1f.dds", 1, ForbiddenCharacter),
        ("textures/x.", 1, TrailingDotOrSpace),
        ("textures/x ", 1, TrailingDotOrSpace),
        ("textures/nUl.dds", 1, ReservedDeviceName),
        ("textures/COM1 .dds", 1, ReservedDeviceName),
        ("textures/LPT9", 1, ReservedDeviceName),
        ("CONIN$.txt", 0, ReservedDeviceName),
    ] {
        let Err(error) = RelativePath::parse(spelling) else {
            panic!("admitted invalid path {spelling:?}");
        };
        assert!(
            matches!(&error, PathError::InvalidComponent { index: actual_index, reason: actual_reason }
                if *actual_index == index && *actual_reason == reason),
            "wrong path error for {spelling:?}: {error:?}; expected component {index}: {reason:?}"
        );
    }
}
