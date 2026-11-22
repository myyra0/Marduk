# Marduk

A cross-platform mod manager: profiles, a mod library, load order and conflict
handling, deployed into the game folder with a recovery journal.

## Platform support

Windows and Linux. The desktop app runs on both; each game, mod source and
deploy method plugs into the same core.

## Known limitations

If a deploy is interrupted on Linux, Marduk recovers it when it next starts. On
Windows, Marduk uses standard Windows flushing, so a power loss in the middle of
a deploy can leave the game folder needing review. Marduk reports what it finds
instead of claiming the deploy succeeded. Windows deploy is not enabled yet: it
still needs the end-to-end recovery run that Linux has.
