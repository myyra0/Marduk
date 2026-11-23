# Marduk

A cross-platform mod manager: profiles, a mod library, load order and conflict
handling, deployed into the game folder with a recovery journal.

## Platform support

Windows and Linux. Marduk runs on Linux; the Windows native build and its unit
tests pass, but a launched desktop shell there has not been verified yet. Each
game, mod source and deploy method plugs into the same core.

## Known limitations

If a deploy is interrupted on Linux, the next start tries to repair the recorded
work and tells you what it found, including anything it could not finish. This
has been tested by restarting the app; it has not been tested by crashing the
app or cutting power. On Windows, Marduk uses standard Windows flushing, so a
power loss in the middle of a deploy can leave the game folder needing review;
Windows uses the narrower guarantee in
[ADR 0004](docs/adr/0004-windows-writes-use-standard-flush.md). Marduk reports
what it finds instead of claiming the deploy succeeded. Windows import and
deploy are enabled on NTFS by the recorded whole-composition run.
