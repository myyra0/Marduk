//! Pure priority resolution over synthetic manifests. No filesystem, archive or
//! network access: inputs are normalized logical targets, output is a plan.

use std::collections::{BTreeMap, BTreeSet};

use crate::ids::{InstalledRevisionId, MemberId, ProfileRevision};
use crate::profiles::ProfileSnapshot;
use crate::targets::TargetFile;

#[derive(Debug)]
pub struct ResolvedInputs {
    pub manifests: BTreeMap<InstalledRevisionId, Vec<TargetFile>>,
}

#[derive(Debug)]
pub struct ResolvedFiles {
    pub revision: ProfileRevision,
    pub entries: Vec<ResolvedEntry>,
}

#[derive(Debug)]
pub struct ResolvedEntry {
    pub target: TargetFile,
    pub candidates: Vec<FileCandidate>,
}

#[derive(Debug)]
pub struct FileCandidate {
    pub member: MemberId,
    pub installed: InstalledRevisionId,
    pub input: TargetFile,
}

#[derive(Debug)]
pub enum ResolveError {
    DuplicateMember(MemberId),
    MissingManifest(InstalledRevisionId),
    AliasedEntries {
        installed: InstalledRevisionId,
        first: TargetFile,
        second: TargetFile,
    },
    ConflictingTargetShape {
        first: TargetFile,
        second: TargetFile,
    },
}

/// Canonical comparison identity: root role plus ASCII-lowercased components.
/// It is not a native filesystem identity.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct CanonicalTarget {
    root: u8,
    components: Vec<String>,
}

impl CanonicalTarget {
    fn of(target: &TargetFile) -> Self {
        Self {
            root: target.root.canonical_key(),
            components: target
                .relative
                .as_str()
                .split('/')
                .map(str::to_ascii_lowercase)
                .collect(),
        }
    }

    fn ancestor(&self, depth: usize) -> Self {
        Self {
            root: self.root,
            components: self.components[..depth].to_vec(),
        }
    }
}

struct Group {
    target: TargetFile,
    candidates: Vec<FileCandidate>,
}

pub fn resolve_files(
    profile: &ProfileSnapshot,
    inputs: &ResolvedInputs,
) -> Result<ResolvedFiles, ResolveError> {
    let mut seen = BTreeSet::new();
    for member in &profile.members {
        if !seen.insert(member.id) {
            return Err(ResolveError::DuplicateMember(member.id));
        }
    }

    let mut aliases_checked = BTreeSet::new();
    let mut groups: BTreeMap<CanonicalTarget, Group> = BTreeMap::new();

    for member in profile.members.iter().filter(|member| member.enabled) {
        let manifest = inputs
            .manifests
            .get(&member.installed)
            .ok_or(ResolveError::MissingManifest(member.installed))?;

        if aliases_checked.insert(member.installed) {
            let mut firsts: BTreeMap<CanonicalTarget, TargetFile> = BTreeMap::new();
            for target in manifest {
                let key = CanonicalTarget::of(target);
                if let Some(first) = firsts.get(&key) {
                    return Err(ResolveError::AliasedEntries {
                        installed: member.installed,
                        first: first.clone(),
                        second: target.clone(),
                    });
                }
                firsts.insert(key, target.clone());
            }
        }

        for target in manifest {
            let key = CanonicalTarget::of(target);
            let group = groups.entry(key).or_insert_with(|| Group {
                target: target.clone(),
                candidates: Vec::new(),
            });
            group.target = target.clone();
            group.candidates.push(FileCandidate {
                member: member.id,
                installed: member.installed,
                input: target.clone(),
            });
        }
    }

    for (key, group) in &groups {
        for depth in 0..key.components.len() {
            if let Some(ancestor) = groups.get(&key.ancestor(depth)) {
                return Err(ResolveError::ConflictingTargetShape {
                    first: ancestor.target.clone(),
                    second: group.target.clone(),
                });
            }
        }
    }

    Ok(ResolvedFiles {
        revision: profile.revision,
        entries: groups
            .into_iter()
            .map(|(_, group)| ResolvedEntry {
                target: group.target,
                candidates: group.candidates,
            })
            .collect(),
    })
}
