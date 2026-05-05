#!/usr/bin/env bash
# Certbot deploy-hook for local.redlinks.fr — re-bundles the helper .deb when
# the Let's Encrypt cert renews and pushes a patch release.
#
# Install:
#   sudo cp cert-renew-hook.sh /etc/letsencrypt/renewal-hooks/deploy/redstars-helper.sh
#   sudo chmod +x /etc/letsencrypt/renewal-hooks/deploy/redstars-helper.sh
#
# Trigger: every successful certbot renewal (3 months by default) runs this.
# It bumps the patch version of the helper, builds a release, and pushes a
# git tag → GitHub Actions picks up the tag and publishes the new release
# (signed Linux/macOS/Windows artifacts via the release.yml workflow).

set -euo pipefail

REPO="${HELPER_REPO:-/home/delminator/redstars/apps/helper-tauri}"
DEMO="${DEMO_DIR:-/home/delminator/redstars/scripts/image-autoencoder/output/demo}"
CERT_LIVE="${RENEWED_LINEAGE:-/etc/letsencrypt/live/local.redlinks.fr}"

# Only run when our domain renewed (certbot calls this hook for every cert).
case "${RENEWED_DOMAINS:-}" in
  *local.redlinks.fr*) ;;
  *) echo "[helper-renew] not our domain (RENEWED_DOMAINS=${RENEWED_DOMAINS:-}) — skip"; exit 0 ;;
esac

echo "[helper-renew] new cert at $CERT_LIVE — repackaging helper"

# 1. Copy fresh cert into the demo dir (which the .deb bundles as resources)
install -m 644 "$CERT_LIVE/fullchain.pem" "$DEMO/cert.pem"
install -m 600 "$CERT_LIVE/privkey.pem"   "$DEMO/key.pem"
chown delminator:delminator "$DEMO/cert.pem" "$DEMO/key.pem"

# 2. Bump patch version in tauri.conf.json + Cargo.toml
cd "$REPO"
CUR=$(grep -oE '"version": "[0-9]+\.[0-9]+\.[0-9]+"' src-tauri/tauri.conf.json | head -1 | grep -oE '[0-9]+\.[0-9]+\.[0-9]+')
IFS=. read -r MAJ MIN PATCH <<< "$CUR"
NEW="${MAJ}.${MIN}.$((PATCH + 1))"
echo "[helper-renew] $CUR → $NEW"
sed -i "s/\"version\": \"$CUR\"/\"version\": \"$NEW\"/" src-tauri/tauri.conf.json
sed -i "s/^version = \"$CUR\"$/version = \"$NEW\"/" src-tauri/Cargo.toml

# 3. Commit + tag + push (GH Actions release workflow takes it from here)
sudo -u delminator git -C "$REPO" add src-tauri/tauri.conf.json src-tauri/Cargo.toml
sudo -u delminator git -C "$REPO" -c user.email=nikola.grange@gmail.com -c user.name=Delminator commit \
  -m "chore: bump to v$NEW — auto cert renewal $(date -I)"
sudo -u delminator git -C "$REPO" push origin main
sudo -u delminator git -C "$REPO" tag "v$NEW"
sudo -u delminator git -C "$REPO" push origin "v$NEW"
echo "[helper-renew] pushed v$NEW — GH Actions will publish .deb/.dmg/.msi"
