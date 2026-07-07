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

# Register the `redstars-helper://` URL scheme on the .desktop file so
# the dashboard's "Lancer le helper" button can launch the shell from
# a browser. The shell doesn't care what URL it receives — being
# invoked is enough to start the Python helper subprocess.
DESKTOP="/usr/share/applications/Redstars Helper.desktop"
if [ -f "$DESKTOP" ]; then
    # Make sure Exec accepts an arg (xdg-open passes the URL).
    grep -q '^Exec=.*%u' "$DESKTOP" || \
        sed -i 's|^\(Exec=[^%]*\)$|\1 %u|' "$DESKTOP" 2>/dev/null
    # Idempotent MimeType: merge with any existing handler list.
    if ! grep -q '^MimeType=.*x-scheme-handler/redstars-helper' "$DESKTOP"; then
        if grep -q '^MimeType=' "$DESKTOP"; then
            sed -i 's|^MimeType=|MimeType=x-scheme-handler/redstars-helper;|' "$DESKTOP" 2>/dev/null
        else
            echo "MimeType=x-scheme-handler/redstars-helper;" >> "$DESKTOP"
        fi
    fi
    update-desktop-database /usr/share/applications >/dev/null 2>&1 || true
fi

# --- Serial-scale access (udev, Linux only) --------------------------------
# The electronic scale is a USB-serial adapter (CH340 / CP210x / FTDI / PL2303).
# On Linux /dev/ttyUSB* is root:dialout mode 660, so the Helper (running as the
# user) can't open it → "permission denied (udev rule?)". Drop a udev rule that
# grants access, then reload so it applies without reboot/relogin (and re-trigger
# so an already-plugged scale picks up the new mode). Best-effort — never blocks
# the install. Windows COM ports and macOS /dev/cu.* are user-accessible by
# default, so there is nothing equivalent to do there.
UDEV_RULE="/etc/udev/rules.d/99-redstars-scale.rules"
cat > "$UDEV_RULE" 2>/dev/null <<'RULE'
# RedStars electronic scale — USB-serial adapters readable/writable without root.
SUBSYSTEM=="tty", ATTRS{idVendor}=="1a86", MODE="0666"
SUBSYSTEM=="tty", ATTRS{idVendor}=="10c4", MODE="0666"
SUBSYSTEM=="tty", ATTRS{idVendor}=="0403", MODE="0666"
SUBSYSTEM=="tty", ATTRS{idVendor}=="067b", MODE="0666"
RULE
udevadm control --reload-rules >/dev/null 2>&1 || true
udevadm trigger --subsystem-match=tty >/dev/null 2>&1 || true

exit 0
