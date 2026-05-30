#!/bin/sh
# Post-install / post-upgrade cleanup.
#
# Sweeps every real user's `~/.cache/redstars-helper/` to remove
# runtime state that doesn't survive a Helper restart anyway:
#   - refs/  : symlink mounts ("ISO virtuelles") created by a previous
#              session; the targets on disk are untouched, only the
#              symlink tmpdirs go away.
#   - iso/   : leftover artefacts from the old ISO demo flow.
#   - *.tmp  : partial writes from an interrupted auto-update.
#
# We do NOT touch helper.py / helper.version — those are an
# auto-updated cache that may legitimately be newer than the bundle we
# just installed; the shell's script_updater decides what to load.
#
# Tolerant: every user is best-effort, errors never block the install.

set +e

for h in /home/*/; do
    u=$(basename "$h" 2>/dev/null)
    [ -z "$u" ] && continue
    [ "$u" = "lost+found" ] && continue
    d="$h.cache/redstars-helper"
    [ -d "$d" ] || continue
    rm -rf "$d/refs" "$d/iso" >/dev/null 2>&1
    rm -f "$d/helper.tmp" >/dev/null 2>&1
done

exit 0
