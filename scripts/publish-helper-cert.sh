#!/usr/bin/env bash
# Ship the current local.redlinks.fr cert to the whole helper fleet.
#
# Re-embeds the live cert/key into helper.py and cuts a script-py-v* release that
# every helper self-updates (core advertises the newest release via
# /api/v1/agents/script-latest, 15-min cache). Run AFTER certbot renews (the
# deploy-hook already refreshed this machine).
#
# Drift-safe: it bases the new script on the LATEST PUBLISHED helper.py, not the
# repo tree (releases have historically run ahead of main), so the version only
# ever moves forward — never a downgrade.
#
# Usage:  scripts/publish-helper-cert.sh [/path/to/live-cert-dir]
#   cert dir defaults to the local helper's (~/.local/share/fr.redlinks.redstars-helper),
#   which holds cert.pem (fullchain) + key.pem after a renewal.
set -euo pipefail

REPO="delminator/redstars-helper"
CERT_DIR="${1:-$HOME/.local/share/fr.redlinks.redstars-helper}"
CERT="$CERT_DIR/cert.pem"; KEY="$CERT_DIR/key.pem"
[ -f "$CERT" ] && [ -f "$KEY" ] || { echo "cert/key not found in $CERT_DIR" >&2; exit 1; }

# Sanity: cert not expired + cert matches key.
END=$(openssl x509 -noout -enddate -in "$CERT" | cut -d= -f2)
openssl x509 -checkend 0 -noout -in "$CERT" || { echo "refusing: cert already expired ($END)" >&2; exit 1; }
cmd5=$(openssl x509 -noout -pubkey -in "$CERT" | openssl md5)
kmd5=$(openssl pkey -pubout -in "$KEY" | openssl md5)
[ "$cmd5" = "$kmd5" ] || { echo "refusing: cert/key mismatch" >&2; exit 1; }

# Latest published script-py version → base + next patch.
LATEST=$(gh release list -R "$REPO" -L 40 --json tagName -q '[.[].tagName|select(startswith("script-py-v"))]|.[0]')
[ -n "$LATEST" ] || { echo "no script-py release found" >&2; exit 1; }
BASE_VER="${LATEST#script-py-v}"
IFS=. read -r MA MI PA <<< "$BASE_VER"; NEW_VER="${MA}.${MI}.$((PA+1))"
echo "base $LATEST → new script-py-v$NEW_VER (cert valid to $END)"

TMP=$(mktemp -d)
gh release download "$LATEST" -R "$REPO" -p helper.py -O "$TMP/helper.py" --clobber
python3 - "$TMP/helper.py" "$CERT" "$KEY" "$BASE_VER" "$NEW_VER" > "$TMP/helper.new.py" <<'PY'
import base64,re,sys
src=open(sys.argv[1]).read()
cert=base64.b64encode(open(sys.argv[2],'rb').read()).decode()
key =base64.b64encode(open(sys.argv[3],'rb').read()).decode()
base,new=sys.argv[4],sys.argv[5]
s=src
s,a=re.subn(r"^VERSION = '%s'"%re.escape(base),"VERSION = '%s'"%new,s,flags=re.M)
s,b=re.subn(r"_EMBEDDED_CERT_B64 = '[^']*'","_EMBEDDED_CERT_B64 = '%s'"%cert,s)
s,c=re.subn(r"_EMBEDDED_KEY_B64 = '[^']*'","_EMBEDDED_KEY_B64 = '%s'"%key,s)
assert a==1 and b==1 and c==1, "subs V=%d C=%d K=%d"%(a,b,c)
import ast; ast.parse(s)
sys.stdout.write(s)
PY

# Verify the rebuilt file's embedded cert really is the fresh one.
emb=$(grep -oE "_EMBEDDED_CERT_B64 = '[^']*'" "$TMP/helper.new.py" | sed "s/.*= '//;s/'//" | base64 -d | openssl x509 -noout -enddate | cut -d= -f2)
[ "$emb" = "$END" ] || { echo "verify failed: embedded=$emb live=$END" >&2; exit 1; }

# Commit onto origin/main in a throwaway worktree, tag, push.
ROOT=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
git -C "$ROOT" fetch origin main -q
W=$(mktemp -d); git -C "$ROOT" worktree add --detach "$W" origin/main -q
cp "$TMP/helper.new.py" "$W/src-tauri/bundled/helper.py"
cp "$CERT" "$W/src-tauri/bundled/cert.pem"; cp "$KEY" "$W/src-tauri/bundled/key.pem"
git -C "$W" add src-tauri/bundled/helper.py src-tauri/bundled/cert.pem src-tauri/bundled/key.pem
git -C "$W" -c user.name=delminator -c user.email=nikolagrange@gmail.com \
  commit -q -m "fix(cert): renew embedded local.redlinks.fr cert → v$NEW_VER (valid to $END)"
git -C "$W" push origin HEAD:main
git -C "$W" tag "script-py-v$NEW_VER"; git -C "$W" push origin "script-py-v$NEW_VER"
git -C "$ROOT" worktree remove --force "$W"; rm -rf "$TMP"
echo "published script-py-v$NEW_VER — GH Actions will sign+release; helpers self-update within ~15 min."
