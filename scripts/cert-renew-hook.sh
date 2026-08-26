#!/usr/bin/env bash
# Certbot deploy-hook for local.redlinks.fr.
#
# Runs after every successful renewal of the helper's HTTPS cert. Its ONE job
# is to keep THIS machine working: drop the fresh cert into the running helper's
# read paths and restart it so browser<->helper HTTPS keeps validating. It does
# NOT ship to the fleet — that is a reviewed step: run
#   scripts/publish-helper-cert.sh
# which re-embeds the cert into helper.py and cuts a script-py release that every
# helper self-updates. (History: the old hook used stale /home/delminator/redstars
# paths — missing the /red/ segment — and only touched the on-disk bundled cert,
# never the EMBEDDED one, so self-updated helpers kept serving the dead cert.)
#
# Install:
#   sudo install -m755 scripts/cert-renew-hook.sh \
#     /etc/letsencrypt/renewal-hooks/deploy/redstars-helper.sh
set -euo pipefail

case "${RENEWED_DOMAINS:-}" in
  *local.redlinks.fr*) ;;
  *) echo "[helper-renew] not our domain — skip"; exit 0 ;;
esac

LIVE="${RENEWED_LINEAGE:-/etc/letsencrypt/live/local.redlinks.fr}"
USER_NAME="${HELPER_USER:-delminator}"
HELPER_DIR="/home/${USER_NAME}/.local/share/fr.redlinks.redstars-helper"
TMP_CERTS="/tmp/redstars-helper-certs"

for d in "$HELPER_DIR" "$TMP_CERTS"; do
  [ -d "$d" ] || continue
  install -m644 -o "$USER_NAME" -g "$USER_NAME" "$LIVE/fullchain.pem" "$d/cert.pem"
  install -m600 -o "$USER_NAME" -g "$USER_NAME" "$LIVE/privkey.pem"   "$d/key.pem"
  echo "[helper-renew] refreshed cert in $d"
done

# Restart the running helper (Tauri child) so it reloads the SSLContext. The
# Tauri parent respawns it; harmless if it isn't running.
pkill -u "$USER_NAME" -f 'fr.redlinks.redstars-helper/helper.py' 2>/dev/null || true

END=$(openssl x509 -noout -enddate -in "$LIVE/fullchain.pem" 2>/dev/null | cut -d= -f2)
echo "[helper-renew] local cert refreshed (valid to ${END})."
echo "[helper-renew] FLEET: run redstars/apps/helper-tauri/scripts/publish-helper-cert.sh to ship to all users."
