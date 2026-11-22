//! The single record of which configurations are available.
//!
//! Availability is a claim about runtime evidence, so it is written down once,
//! here, as [`PROOFS`]: for a `(capability, operating system, volume class)`
//! triple, the proof layer reached and the tests, date and revision that
//! produced it. Gates ask [`available`]; they never infer availability from a
//! filesystem name on their own.
//!
//! The platform adapter reports only the observed [`VolumeClass`] (Linux
//! `fstatfs` magic, Windows volume name) plus whether its code for that
//! operating system exists. It does not decide product availability.
//!
//! `crates/core/tests/availability_guard.rs` parses
//! `docs/research/m1-native-results.md` and fails when the record and the
//! evidence disagree in either direction, so a gate cannot be opened without a
//! run behind it.

use std::fmt;

/// Operating system a proof was observed on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Os {
    Linux,
    Windows,
}

/// Filesystem class of the observed volume, as the adapter reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum VolumeClass {
    Btrfs,
    Tmpfs,
    Ext4,
    Ntfs,
    /// Any other or unrecognised volume, including an unreadable root.
    Other,
}

/// Capability whose availability a gate asks about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Capability {
    /// Private library staging: archive admission and the hashed payload under
    /// `revisions/<id>/`.
    LibraryStaging,
    /// File-level native effect primitives (copy, publish, replace, detach,
    /// directory sync).
    EffectPrimitives,
    /// The composed native deployment adapter: journal plus effect primitives,
    /// replay and restore.
    NativeEffects,
}

/// Consumer whose requirement is being answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// The adapter itself may run; no client-visible write is implied.
    Adapter,
    /// A client library import.
    Import,
    /// A client deploy/apply.
    Deploy,
    /// A client restore.
    Restore,
}

/// Evidence rung a proof reached. A proof satisfies every lower threshold.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Layer {
    /// The file-level effect primitives ran.
    Primitives,
    /// The private staging adapter ran.
    Staging,
    /// The composed native deployment adapter ran in the proof lane.
    Adapter,
    /// Publication through the production catalog ran.
    Catalog,
    /// A client-visible deploy or restore ran.
    Deploy,
}

impl fmt::Display for Capability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::LibraryStaging => "library staging",
            Self::EffectPrimitives => "effect primitives",
            Self::NativeEffects => "native effects",
        })
    }
}

impl fmt::Display for VolumeClass {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Btrfs => "Btrfs",
            Self::Tmpfs => "tmpfs",
            Self::Ext4 => "ext4",
            Self::Ntfs => "NTFS",
            Self::Other => "an unrecognised volume",
        })
    }
}

impl fmt::Display for Os {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Linux => "Linux",
            Self::Windows => "Windows",
        })
    }
}

impl fmt::Display for Layer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Primitives => "primitives",
            Self::Staging => "staging",
            Self::Adapter => "adapter",
            Self::Catalog => "catalog",
            Self::Deploy => "deploy",
        })
    }
}

/// One recorded runtime proof for a capability on one configuration.
#[derive(Clone, Copy, Debug)]
pub struct Proof {
    pub capability: Capability,
    pub os: Os,
    pub volume: VolumeClass,
    pub layer: Layer,
    /// Test functions that ran for this proof. Each must be defined in the
    /// tree; `crates/core/tests/availability_guard.rs` enforces that.
    pub witnesses: &'static [&'static str],
    /// Local date of the run, `YYYY-MM-DD`.
    pub date: &'static str,
    /// Revision the run was observed at.
    pub revision: &'static str,
}

/// Code that exists on an operating system. Implementation is not evidence.
const IMPLEMENTED: &[(Capability, Os)] = &[
    (Capability::LibraryStaging, Os::Linux),
    (Capability::LibraryStaging, Os::Windows),
    (Capability::EffectPrimitives, Os::Linux),
    (Capability::EffectPrimitives, Os::Windows),
    (Capability::NativeEffects, Os::Linux),
    (Capability::NativeEffects, Os::Windows),
];

/// Every recorded proof. An entry here is a claim that a test ran on that
/// configuration; the guard test holds it to the evidence document.
pub const PROOFS: &[Proof] = &[
    Proof {
        capability: Capability::LibraryStaging,
        os: Os::Linux,
        volume: VolumeClass::Btrfs,
        layer: Layer::Catalog,
        witnesses: &["qualified_volume_stages_the_payload_through_the_production_catalog"],
        date: "2026-11-22",
        revision: "a2250e9",
    },
    Proof {
        capability: Capability::LibraryStaging,
        os: Os::Linux,
        volume: VolumeClass::Tmpfs,
        layer: Layer::Catalog,
        witnesses: &["qualified_volume_stages_the_payload_through_the_production_catalog"],
        date: "2026-11-22",
        revision: "a2250e9",
    },
    Proof {
        capability: Capability::LibraryStaging,
        os: Os::Windows,
        volume: VolumeClass::Ntfs,
        layer: Layer::Staging,
        witnesses: &[
            "redirected_revisions_is_refused_without_writes",
            "redirected_revision_path_is_refused_without_writes",
            "redirected_payload_during_owner_record_is_refused_without_writes",
            "reclaim_removes_a_verified_private_stage",
            "reobserve_refuses_a_revision_redirected_while_unbound",
            "reobserve_refuses_an_added_payload_file",
            "reobserve_refuses_a_changed_bound_entry",
            "staged_revision_keeps_real_bytes_private_and_reobserves_after_reopen",
        ],
        date: "2026-11-22",
        revision: "367ca69",
    },
    Proof {
        capability: Capability::EffectPrimitives,
        os: Os::Linux,
        volume: VolumeClass::Btrfs,
        layer: Layer::Primitives,
        witnesses: &[
            "native_no_replace_honors_concurrent_create",
            "native_replace_records_actually_displaced_file",
            "native_rejects_root_and_parent_substitution",
            "native_detach_preserves_unknown_file",
            "native_error_does_not_prove_no_change",
        ],
        date: "2026-11-22",
        revision: "29e587d",
    },
    Proof {
        capability: Capability::EffectPrimitives,
        os: Os::Linux,
        volume: VolumeClass::Tmpfs,
        layer: Layer::Primitives,
        witnesses: &[
            "native_no_replace_honors_concurrent_create",
            "native_replace_records_actually_displaced_file",
            "native_rejects_root_and_parent_substitution",
            "native_detach_preserves_unknown_file",
            "native_error_does_not_prove_no_change",
        ],
        date: "2026-11-22",
        revision: "29e587d",
    },
    Proof {
        capability: Capability::EffectPrimitives,
        os: Os::Windows,
        volume: VolumeClass::Ntfs,
        layer: Layer::Primitives,
        witnesses: &[
            "copy_preserves_bytes_and_last_write_time_and_refuses_existing",
            "publish_refuses_an_existing_target_and_leaves_it_intact",
            "replace_retains_the_displaced_file_and_keeps_the_staged_object",
            "detach_preserves_an_unknown_file",
            "sync_directory_succeeds_on_a_real_directory",
        ],
        date: "2026-11-22",
        revision: "eb39be0",
    },
    Proof {
        capability: Capability::NativeEffects,
        os: Os::Linux,
        volume: VolumeClass::Btrfs,
        layer: Layer::Adapter,
        witnesses: &[
            "qualified_native_round_trip_retains_backups_and_restores_the_original_tree",
            "restart_after_apply_before_ack_rolls_back_and_never_reapplies",
            "restart_mid_restore_continues_in_reverse_order",
        ],
        date: "2026-11-22",
        revision: "101b4f7",
    },
    Proof {
        capability: Capability::NativeEffects,
        os: Os::Linux,
        volume: VolumeClass::Tmpfs,
        layer: Layer::Adapter,
        witnesses: &[
            "qualified_native_round_trip_retains_backups_and_restores_the_original_tree",
            "restart_after_apply_before_ack_rolls_back_and_never_reapplies",
            "restart_mid_restore_continues_in_reverse_order",
        ],
        date: "2026-11-22",
        revision: "101b4f7",
    },
    Proof {
        capability: Capability::NativeEffects,
        os: Os::Linux,
        volume: VolumeClass::Btrfs,
        layer: Layer::Deploy,
        witnesses: &["deploy_round_trip_retains_and_restores_on_linux_tmpfs_or_btrfs"],
        date: "2026-11-22",
        revision: "59733a3",
    },
    Proof {
        capability: Capability::NativeEffects,
        os: Os::Linux,
        volume: VolumeClass::Tmpfs,
        layer: Layer::Deploy,
        witnesses: &["deploy_round_trip_retains_and_restores_on_linux_tmpfs_or_btrfs"],
        date: "2026-11-22",
        revision: "59733a3",
    },
];

/// The evidence rung a route requires for a capability.
fn required_layer(capability: Capability, route: Route) -> Layer {
    match (capability, route) {
        (Capability::LibraryStaging, Route::Adapter) => Layer::Staging,
        (Capability::LibraryStaging, Route::Import) => Layer::Catalog,
        (Capability::EffectPrimitives, Route::Adapter) => Layer::Primitives,
        (Capability::NativeEffects, Route::Adapter) => Layer::Adapter,
        // No client-visible import or deployment route consumes the effect or
        // adapter capabilities directly; they are answered at the deploy rung.
        (_, Route::Import) => Layer::Catalog,
        (_, Route::Deploy | Route::Restore) => Layer::Deploy,
    }
}

/// What the record says about one configuration and consumer.
#[derive(Clone, Copy, Debug)]
pub struct Qualification {
    /// Whether code exists for this capability on this operating system.
    pub implemented: bool,
    /// The proof that satisfies the route, when the evidence reaches its rung.
    pub proof: Option<&'static Proof>,
}

impl Qualification {
    pub fn available(&self) -> bool {
        self.proof.is_some()
    }
}

/// Answer a `(capability, os, volume class, route)` question from the record.
pub fn qualification(
    capability: Capability,
    route: Route,
    os: Os,
    volume: VolumeClass,
) -> Qualification {
    let required = required_layer(capability, route);
    Qualification {
        implemented: IMPLEMENTED.contains(&(capability, os)),
        proof: PROOFS.iter().find(|proof| {
            proof.capability == capability
                && proof.os == os
                && proof.volume == volume
                && proof.layer >= required
        }),
    }
}

/// Whether the record proves a capability for the route on this configuration.
pub fn available(capability: Capability, route: Route, os: Os, volume: VolumeClass) -> bool {
    qualification(capability, route, os, volume).available()
}

/// The first witness a proof names that `defined` does not recognise.
///
/// The guard passes a test-name lookup; a proof that names a test no longer in
/// the tree is stale evidence and must be corrected or withdrawn.
pub fn missing_witness(proof: &Proof, defined: &dyn Fn(&str) -> bool) -> Option<&'static str> {
    proof.witnesses.iter().copied().find(|name| !defined(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_available(capability: Capability, route: Route, os: Os, volume: VolumeClass) -> bool {
        available(capability, route, os, volume)
    }

    #[test]
    fn library_import_is_proven_on_both_qualified_linux_volumes() {
        for volume in [VolumeClass::Btrfs, VolumeClass::Tmpfs] {
            let qualification =
                qualification(Capability::LibraryStaging, Route::Import, Os::Linux, volume);
            assert!(qualification.implemented);
            assert!(qualification.available(), "{volume} library import");
        }
    }

    #[test]
    fn windows_staging_adapter_runs_while_its_import_stays_closed() {
        assert!(is_available(
            Capability::LibraryStaging,
            Route::Adapter,
            Os::Windows,
            VolumeClass::Ntfs
        ));
        assert!(!is_available(
            Capability::LibraryStaging,
            Route::Import,
            Os::Windows,
            VolumeClass::Ntfs
        ));
    }

    #[test]
    fn native_effects_adapter_is_proven_only_on_the_two_linux_volumes() {
        for volume in [VolumeClass::Btrfs, VolumeClass::Tmpfs] {
            assert!(is_available(
                Capability::NativeEffects,
                Route::Adapter,
                Os::Linux,
                volume
            ));
        }
        assert!(!is_available(
            Capability::NativeEffects,
            Route::Adapter,
            Os::Windows,
            VolumeClass::Ntfs
        ));
        assert!(!is_available(
            Capability::NativeEffects,
            Route::Adapter,
            Os::Linux,
            VolumeClass::Ext4
        ));
    }

    #[test]
    fn effect_primitives_are_proven_on_all_three_witnessed_volumes() {
        for (os, volume) in [
            (Os::Linux, VolumeClass::Btrfs),
            (Os::Linux, VolumeClass::Tmpfs),
            (Os::Windows, VolumeClass::Ntfs),
        ] {
            let qualification =
                qualification(Capability::EffectPrimitives, Route::Adapter, os, volume);
            assert!(qualification.available(), "{os}/{volume} primitives");
            assert_eq!(
                qualification.proof.map(|proof| proof.layer),
                Some(Layer::Primitives)
            );
        }
    }

    #[test]
    fn deploy_and_restore_follow_the_witnessed_deploy_evidence() {
        let witnessed = [
            (Os::Linux, VolumeClass::Btrfs),
            (Os::Linux, VolumeClass::Tmpfs),
        ];
        for capability in [
            Capability::LibraryStaging,
            Capability::EffectPrimitives,
            Capability::NativeEffects,
        ] {
            for os in [Os::Linux, Os::Windows] {
                for volume in [
                    VolumeClass::Btrfs,
                    VolumeClass::Tmpfs,
                    VolumeClass::Ext4,
                    VolumeClass::Ntfs,
                    VolumeClass::Other,
                ] {
                    let expected = capability == Capability::NativeEffects
                        && witnessed.contains(&(os, volume));
                    for route in [Route::Deploy, Route::Restore] {
                        assert_eq!(
                            is_available(capability, route, os, volume),
                            expected,
                            "{capability} {route:?} on {os}/{volume}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn an_unknown_volume_is_unproven() {
        for capability in [
            Capability::LibraryStaging,
            Capability::EffectPrimitives,
            Capability::NativeEffects,
        ] {
            for os in [Os::Linux, Os::Windows] {
                for route in [Route::Adapter, Route::Import, Route::Deploy, Route::Restore] {
                    let qualification = qualification(capability, route, os, VolumeClass::Other);
                    assert!(qualification.implemented);
                    assert!(!qualification.available());
                }
            }
        }
    }

    #[test]
    fn a_witness_that_no_test_defines_is_reported() {
        const PROOF: Proof = Proof {
            capability: Capability::LibraryStaging,
            os: Os::Linux,
            volume: VolumeClass::Btrfs,
            layer: Layer::Catalog,
            witnesses: &["this_test_does_not_exist"],
            date: "2026-11-22",
            revision: "0000000",
        };
        let defined = |name: &str| {
            name == "qualified_volume_stages_the_payload_through_the_production_catalog"
        };
        assert_eq!(
            missing_witness(&PROOF, &defined),
            Some("this_test_does_not_exist")
        );
        assert_eq!(missing_witness(&PROOFS[0], &defined), None);
    }

    #[test]
    fn every_proof_names_at_least_one_witness_at_its_layer() {
        for proof in PROOFS {
            assert!(!proof.witnesses.is_empty(), "{proof:?}");
            let route = if proof.layer == Layer::Deploy {
                Route::Deploy
            } else {
                Route::Adapter
            };
            assert_eq!(
                qualification(proof.capability, route, proof.os, proof.volume)
                    .proof
                    .map(|proven| proven.layer),
                Some(proof.layer)
            );
        }
    }
}
