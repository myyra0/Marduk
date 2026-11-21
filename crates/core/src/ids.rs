//! Opaque local identities. Storage owns allocation and serialization; numeric
//! order is never file priority.

macro_rules! local_id {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
        pub struct $name(u64);

        impl $name {
            pub fn new(value: u64) -> Self {
                Self(value)
            }

            pub fn get(self) -> u64 {
                self.0
            }
        }
    };
}

local_id!(ProfileId);
local_id!(ProfileRevision);
local_id!(MemberId);
local_id!(InstalledRevisionId);
