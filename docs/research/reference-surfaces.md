# Pinned reference surface notes

**Evidence level:** static inspection from the [inventory baseline](../inventory.md). These lists identify concrete audit candidates; they do not promise that every code path is shipped, reachable, or verified.

## Game handlers

`src/Games/meson.build` lists 31 shipped-source directories, including Custom. `src/Utils/games/discovery.py:270–409` loads module-local concrete `BaseGame` subclasses, then user custom definitions, manager-plugin games, and saved instance overlays. Imported/abstract classes and files rejected by the loader do not count as registrations. AppImage/Flatpak/source roots may differ.

The 72 literal candidates below come from `game_id` return values, definition dictionaries or keyword arguments under the game-family directories. Check for computed IDs, package omissions and runtime name collisions before declaring coverage complete. Names in backticks identify reference fixtures, not Marduk's public IDs.

| Implementation/fixture group | Pinned family and candidate IDs | Special case to prove |
|---|---|---|
| Bethesda | `Fallout3`, `Fallout3GOTY`, `Fallout4`, `Fallout4VR`, `Fallout76`, `FalloutNC`, `FalloutNV`, `Oblivion`, `Starfield`, `enderal`, `enderalse`, `skyrim`, `skyrim_se`, `skyrimvr` | Base/edition store IDs, profile INIs/saves, order and masters, archive/loose policy, directory casing, loader versions. For `skyrim_se`, fixture root has `SkyrimSELauncher.exe` and `Data`; actual direct launch uses `SkyrimSE.exe`. |
| Morrowind / remaster | `morrowind`, `morrowind_openmw`, `oblivion_remastered` | OpenMW can target a profile/config root outside the game; remaster routes into nested data. Neither is proven by a plain Bethesda `Data` fixture. |
| BepInEx | `Hollow_Knight_Silksong`, `Lethal_Company`, `Subnautica`, `Subnautica_Below_Zero`, `Supermarket_Simulator`, `TCG_Card_Shop_Simulator`, `Valheim`, `dysonsphereprogram`, `graveyard_keeper_2`, `inscryption`, `nivalis_nights`, `peak`, `repo`, `riskofrain2`, `v_rising` | Framework versus content, package namespacing, dependencies, install destinations and launch behavior per title. |
| RE Engine invalidation | `devil_may_cry_5`, `dragons_dogma_2`, `resident_evil_2`, `resident_evil_3`, `resident_evil_4`, `resident_evil_7`, `resident_evil_village`, `street_fighter_6` | Invalidation or patch-back witness, archive ownership, failure rollback; don't assume every game uses the same physical side effect. |
| RE Engine loose loading | `Pragmata`, `monster_hunter_rise`, `monster_hunter_wilds`, `resident_evil_requiem` | Resolved loose paths and generated tool outputs in both supported views. |
| Dedicated routing / loaders | `baldurs_gate_3`, `cyberpunk_2077`, `elden_ring`, `nightreign`, `red_dead_redemption_2`, `witcher_3`, `Stardew_Valley`, `No_Man_s_Sky`, `mewgenics`, `Prey_2017` | Manifest/game options outside install root where applicable, mod.io/SMAPI/external loader as applicable, exact rollback. Prey's Steam executable differs from its GOG/Epic paths. |
| Additional root/config handlers | `7_Days_to_Die`, `Balatro`, `daggerfall_unity`, `darktide`, `dragon_age_origins`, `Hades`, `Hades_II`, `jagged_alliance_3`, `kingdom_come_deliverance`, `kingdom_come_deliverance_2`, `marvel_rivals`, `Mount___Blade_II__Bannerlord`, `Project_Zomboid`, `Rimworld`, `Slay_The_Spire_2`, `the_sims_3`, `the_sims_4`, `Voices_Of_The_Void` | Check deploy root, mod layout/archives, discovery/edition, launch and runtime file expectations per title. Group only when those behaviors really match. |
| Custom | Definitions rather than a fixed set of title IDs | Data-only validation and user-visible sharing without running untrusted extension code. |

## Packaged wizard candidates

The filenames below correspond to `src/wizards_qt/meson.build`. Use the named view together with a game handler's wizard list and the invoked implementation to prove reachability. `_view_base`, `_mod_loader_installer_view`, `bodyslide_output`, `proton_step` and `ReShade.ini` are listed helpers/assets, not additional standalone capabilities.

- **Game setup and complex installation:** `bethesda_register`, `bg3_import`, `curated_profile`, `dtkit_patch`, `fallout_downgrade`, `mulderload_downgrader`, `fnv_4gb`, `new_california`, `oblivion_4gb`, `ttw`.
- **Frameworks and game changes:** `bg3le_install`, `engine_fixes`, `esm_fixes`, `me3_install`, `mscloader`, `re_pak_restore`, `reshade`, `script_extender`, `smapi`, `special_k`, `srml`.
- **Executables/configuration:** `bethini`, `bodyslide`, `bodyslide_linux`, `cao`, `creationkit`, `easynpc`, `eet`, `eslifier`, `modio_settings`, `pandora`, `pgpatcher`, `regulation_merge`, `script_merger`, `sdt`, `synthesis`, `wrye_bash`, `xedit`, `xtranslator`.
- **LOD and assets:** `acmos`, `autoblend`, `autoseasons`, `bsa_decompressor`, `bsa_pack_candidates`, `dyndolod`, `gpak`, `skygen`, `snowfixer`, `texture_tool`, `yeetpatch`.
- **Inspection:** `nif_viewer`, `npc_viewer`, `plugin_audit`.

## External and packaged entry points

- `src/cli.py:427–535` registers game/profile lists, deploy, restore, launch, ownership-map export, credential clear, plugin validation (excluded), and stdio headless. The launcher also accepts a hidden package bridge; preserve the external launch outcome, not the argument name.
- The headless registry is a better coverage denominator than `docs/headless.md` alone. For each registration, record `effect`, write targets, job/pause/cancel policy, visible trigger, and replacement or exclusion. Separate one user capability presented via multiple operations from multiple capabilities hidden behind one operation. Review `manager_plugins` cases individually; leave the host out without losing built-in provider behavior.
- `src/gui_qt/README.md` and the package `meson.build` files give the desktop map. Follow menu/context/shortcut action registrations into core or tool modules to find behaviors with no headless method.
- `src/gui_qt/collections/create.py:1187–1199` calls upload plus create/revision operations in `src/Nexus/nexus_api.py`. Authoring is therefore not just a README claim. It is a separate consent/authorization path from installation.
- `src/Utils/mo2/{dummy,stub}.py` writes limited tool-facing instance data for tools such as PGPatcher and ESLifier. That is output interoperability, not a requirement to import old installations or adopt another manager's storage internally.
- AppImage, Flatpak and Nix recipes are separately visible in the pinned tree. The [package route evidence](package-route-evidence.md) distinguishes recipes, install origins, upgrade ownership and actual checks. See also the [independent first-run catalog snapshot](first-run-catalog.md) for data and optional extensions, not packaged runtime claims.

## Desktop audit entry points

Static search for `QAction`, `addAction`, or `addMenu` found **28 files** under `src/gui_qt` at the baseline. This is a trace seed, not a complete registration denominator: signals, toolbar buttons, dynamically generated menus, shortcuts and widget actions can bypass all three forms. Use `app.py` and feature glue for those surfaces.

| Entry-point group | Candidate pinned paths under `src/gui_qt/` | Inventory or follow-up |
|---|---|---|
| Shell and action framework | `app.py`, `dialogs/text_input.py`, `widgets/{detachable_tabs,filter_menu_button,selector_button}.py`, `runtime/shortcuts.py` | UI shell, settings, key and mouse bindings, panel behavior. Structural widgets may implement no distinct product outcome; check startup, modal and keyboard actions. |
| File and mod editing | `modlist/{menu,view}.py`, `files/{data_view,mod_files_view,overwrite}.py` | `MOD-01`–`MOD-05`, `DEP-03`, `DEP-05`. Include file ownership, multi-selection and boundary promotion. |
| Game, launch, tool options | `games/custom.py`, `launchers/{exe_settings,provider_settings}.py` and wizard registrations | `GAME-02`, `RUN-02`, `RUN-04`, `TOOL-01`, `TOOL-02`. Confirm whether each provider action is native or host-only. |
| Game data and previews | `plugins/{menu,view}.py`, `saves/view.py`, `previews/nif.py` | `DATA-01`, `DATA-02`, `MOD-05`. Exclude only host contributions, not built-in game-plugin administration. |
| Providers and downloads | `downloads/view.py`, `nexus/browser.py`, `thunderstore/browser.py`, `collections/{browser,installed}.py`, `wabbajack/view.py` | `SRC-01`–`SRC-04`, `SET-01`–`SET-03`; trace authoring/upload separately and do not infer an operation from browser presence. |
| Support and notifications | `support/notification_center.py` and neighbor actions in `support/` | `APP-04`, `EXT-03`; view, persist, clear and redact as appropriate, no external upload without authorization. |
| Contributions from the manager plugin host | `runtime/{plugin_filters,plugin_menus,plugin_rows,plugin_ui}.py` | `OUT-01` for third-party callbacks. `plugin_filters.py:253,284` also saves search, ordinary column visibility/width/order and sort; retain this as a built-in saved view, not as a plugin-host feature. |

## Formats and other behavior that merits a separate test

- `src/Utils/archives/identify.py` recognizes ZIP, RAR, 7z, TAR, gzip, bzip2, xz and zstd headers. Unknown can be offered to a native extractor in the reference, so a header alone does not define the actual format support list. `archives/listing.py` has a distinct RAR inspection path (rejects password-protected RAR there); `Utils/mods/install.py` chooses native tools and library fallbacks. Require separate inspection and extraction witnesses per enabled format/compression variant, including falsely renamed inputs, and verify packager availability. Marduk should accept an explicit audited list, not send unknown input to an unconditional native fallback.
- `src/Utils/boundaries/manage.py:353,828,890,997` lists and edits the profile's Overwrite or Root Folder, and can move items into a target mod; this is not a general filesystem browser. Map it onto explicit source and destination root roles, selection and revision, and output provenance.
- `src/Utils/collections/install.py:234,274,437,491,3767` distinguishes update policy, authored/preinstalled order and bundled assets. A single example of installing a list cannot prove later revision, manual, append or bundle behavior. Preserve legitimate provider-specific inputs rather than copying the reference's private manifest layout.
- `src/Utils/wabbajack/{manifest,preflight,reconstruct}.py` distinguish directive parsing, game/source authentication by expected bytes, disk destinations and reconstruct/reuse. Build fixtures by directive/side-effect type, not just by a happy-path list. Track rights for separately redistributed patches or helpers; do not assume reference code licensing covers every shipped artifact.
