# ADR 0006: durable installation identity

**Status:** Accepted.

## Context

M1 recorded the Linux mount view ID (`statx.stx_mnt_id`) inside `NativeIdentity::Linux`
and required it for every reopen. That made the recorded installation depend on how the
same filesystem objects happened to be mounted at Setup time. An unmount/remount, reboot,
replug or bind view changed the ID and disabled the installation until Setup was rerun,
even though the filesystem, inode and birth time were unchanged. Mount IDs are also
recycled, so equality on them was neither durable nor a guarantee of identity.

## Decision

- `NativeIdentity::Linux` is the durable object identity only: filesystem magic,
  filesystem ID, Btrfs tree ID when present, inode number and birth time.
- `NativeIdentity::Windows` is unchanged: volume serial, file ID and creation time.
- The mount view is not part of the token, the journal key encoding or any fingerprint.
  The evidence encoding is unversioned: the first byte is the existing Linux/Windows
  identity tag, which already discriminates the payload. There is no installed base to
  migrate.
- Mount IDs remain usable for *runtime* checks of parent and child handles in the
  platform adapter, where both handles are observed in the same session. Those checks
  answer "is this handle still under the bound parent", not "is this the recorded
  installation".

## Rejected alternatives

- **Keep the mount view in equality only.** It still makes a recorded installation
  invalid across a remount, which is the defect being removed. It also forced a relink
  or compatibility path to recover, adding the moving parts this change removes.
- **Keep a version byte only to reject the old layout.** There is no installed data to
  protect; the extra branch, typed error and test were ceremony. See the cost below for
  what that gives up.
- **Keep the field but ignore it during decode.** The token and journal key would still
  carry a session value, so a remount would still produce different keys and comparisons.

## Cost if wrong

The same filesystem objects reached through a different mount view are accepted as the
recorded installation. Every write still verifies the bound parent and target
fingerprints, and fingerprint equality still includes filesystem, tree, inode and birth
time, so a different object with a recycled inode number and matching birth time is the
remaining theoretical collision — the same residual the durable fields already carried.
A stale developer-local journal blob is not tagged as old: structural checks reject most
such blobs, but there is no installed base and no user data to protect. A version byte
can be added cheaply if an installed base ever appears.
