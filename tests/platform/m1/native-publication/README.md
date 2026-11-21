# R03 synthetic native publication witness

Use an existing disposable scratch parent. The runner creates unique children; never supply a game, library, elevated token or prepared disk. Linux runtime is Btrfs and tmpfs only; ext4 not run. Btrfs requires ordinary-user create **and delete** of owned subvolumes; teardown refusal retains the scratch path and counts as not run. Windows requires an ordinary token and an identifiable NTFS volume; a hosted `windows-2025` compile-only job is not runtime.

```sh
python tests/platform/m1/native-publication/run.py "$SCRATCH"
```

Run separately for each qualifying volume. This links the storage witness from the locked workspace without adding a reverse crate dependency. `cargo test --locked -p marduk-platform --test native_publication` alone checks compilation; without the explicit scratch and catalog executable it emits `R03 NOT RUN`. Evaluate subprocess markers and independent evidence, not only libtest's exit or pass count. Process witnesses do not cut power, reset a VM, or qualify filesystem durability from their own survival.
