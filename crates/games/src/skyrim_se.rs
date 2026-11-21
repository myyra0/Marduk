//! Skyrim Special Edition layout.
//!
//! The marker witness is only an inert regular-file check; it does not open or
//! validate a game binary and grants no write authority.

use marduk_core::root_identity::GameLayout;

pub const LAYOUT: GameLayout = GameLayout {
    data: "Data",
    markers: &["SkyrimSELauncher.exe", "SkyrimSE.exe"],
};
