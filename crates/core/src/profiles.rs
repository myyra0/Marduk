//! Ordered profile membership snapshot. The vector is low to high priority.

use crate::ids::{InstalledRevisionId, MemberId, ProfileId, ProfileRevision};

#[derive(Clone, Debug)]
pub struct ProfileSnapshot {
    pub id: ProfileId,
    pub revision: ProfileRevision,
    pub members: Vec<ProfileMember>,
}

#[derive(Clone, Debug)]
pub struct ProfileMember {
    pub id: MemberId,
    pub installed: InstalledRevisionId,
    pub enabled: bool,
}
