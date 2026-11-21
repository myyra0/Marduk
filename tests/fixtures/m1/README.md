# M1 resolution fixture

Pointer to the golden M1 example: [recovery oracle — Golden fixture](../../../docs/research/recovery-oracle.md#golden-fixture).

Two input mods supply the same logical target `Data/textures/winner.dds`: mod A writes `a\n`, mod B writes `b\n`. Lower-priority members come first; the later member wins.

- Members low to high `A, B`: B wins, so the winning target spelling and candidate are B's (`b\n`).
- Members low to high `B, A`: A wins (`a\n`).

This fixture is manifests and logical target paths only. Deployed bytes, publication and restore are not tested here.
