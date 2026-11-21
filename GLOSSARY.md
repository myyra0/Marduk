# Marduk mod management

This vocabulary distinguishes the user's requested mod setup from what a game or tool actually sees.

## Games and profiles

**Game definition**:
A supported title or variant and the rules it needs to be managed.
_Avoid_: Game install, game instance

**Game installation**:
A particular installed copy of a game on a device.
_Avoid_: Game definition, profile

**Managed instance**:
A Marduk workspace for one game installation, with its own profiles and settings.
_Avoid_: Profile, copy of the game

**Profile**:
A named selection of mods, their activation and priority, and settings for a managed instance.
_Avoid_: Deployed state

**Installed revision**:
One recorded version and installation selection of a mod's files.
_Avoid_: Profile entry, current download

**Profile membership**:
The use of an installed revision in one profile, with a position and enabled state.
_Avoid_: Mod installation

**File priority**:
The order that decides which eligible mod supplies a conflicting file; later (lower in the mod list) wins.
_Avoid_: Plugin load order

**Game-content plugin**:
A file interpreted by a game or its loader, sometimes with its own load order.
_Avoid_: Manager plugin

## Deployment and recovery

**Deployment**:
The application of a resolved profile to what the game or tool can see.
_Avoid_: Mod import, profile selection

**Physical deployment**:
Placement of modded files or links in actual game or related directories.
_Avoid_: Private game view

**Private game view**:
The modded tree presented to a managed game or tool without applying that tree to the normal game installation. Separate game-specific writes can still exist and must be disclosed.
_Avoid_: VFS backend, zero-write guarantee

**Restore**:
Undoing the managed changes of a deployment without removing unrelated files.
_Avoid_: Reinstall the game, delete all non-vanilla files

**Captured output**:
A new or changed file attributed to a managed game or tool run and kept for inspection or deliberate reuse.
_Avoid_: Original backup, installed revision

**Recovery conflict**:
A state where Marduk cannot safely complete recovery because the expected managed file and the observed file do not agree.
_Avoid_: Successful restore, permission to overwrite
