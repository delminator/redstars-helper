#!/bin/sh
# Pre-install / pre-upgrade cleanup.
#
# Runs before the new package writes its files. Goal: make sure no old
# Redstars Helper process is holding bundled resources open or
# squatting on the helper's TCP ports, otherwise the new install would
# either fail mid-copy or come up next to a stale instance and the user
# would see "two helpers, neither works".
#
# Tolerant by design: every step is best-effort (`|| true`), the script
# never aborts an install on a kill failure.

set +e

# Tauri shell binary (deb/rpm install path).
pkill -TERM -f /usr/bin/redstars-helper >/dev/null 2>&1
# Python subprocess spawned by the shell, both possible script paths
# (bundled fallback + per-user cached auto-updated copy).
pkill -TERM -f "python.*Redstars Helper/bundled/helper\.py" >/dev/null 2>&1
pkill -TERM -f "python.*\.cache/redstars-helper/helper\.py" >/dev/null 2>&1

# Give them a beat to exit cleanly, then SIGKILL stragglers.
sleep 1
pkill -KILL -f /usr/bin/redstars-helper >/dev/null 2>&1
pkill -KILL -f "python.*Redstars Helper/bundled/helper\.py" >/dev/null 2>&1
pkill -KILL -f "python.*\.cache/redstars-helper/helper\.py" >/dev/null 2>&1

exit 0
