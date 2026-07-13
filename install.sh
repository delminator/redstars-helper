#!/bin/sh
# RedStars Helper — installeur / mise à jour universel.
#
#   curl -fsSL https://raw.githubusercontent.com/delminator/redstars-helper/main/install.sh | sh
#
# Le même script installe ET met à jour : relancez-le pour passer à la dernière
# version. Il choisit tout seul le bon format selon la machine :
#
#   - OS immuable (Fedora Silverblue/Kinoite, uBlue…) ou /usr en lecture seule
#     → AppImage, posée dans ~/.local/lib (aucune écriture système requise).
#   - Debian / Ubuntu / Mint  → paquet .deb via apt/dpkg (sudo).
#   - Fedora / RHEL / openSUSE → paquet .rpm via dnf/zypper/rpm (sudo).
#
# Forcer un format : `… | sh -s -- --appimage` (ou --deb / --rpm).
# Désinstaller     : `… | sh -s -- --uninstall`.
#
# Les paquets .deb/.rpm apportent en plus la règle udev de la balance série,
# la dépendance python3 et l'enregistrement du schéma redstars-helper://.
# L'AppImage, elle, est le seul format qui se met à jour tout seul ensuite et
# le seul qui marche sur un OS immuable.

set -eu

REPO="delminator/redstars-helper"
API="https://api.github.com/repos/$REPO/releases/latest"
APPIMAGE_DIR="$HOME/.local/lib/redstars-helper"
APPIMAGE_PATH="$APPIMAGE_DIR/Redstars.Helper.AppImage"
BIN_DIR="$HOME/.local/bin"
DESKTOP_FILE="$HOME/.local/share/applications/redstars-helper.desktop"
LAUNCHER_MARKER="# redstars-helper launcher (auto-généré)"

log()  { printf '\033[36m›\033[0m %s\n' "$*"; }
warn() { printf '\033[33m!\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[31m✗\033[0m %s\n' "$*" >&2; exit 1; }

usage() {
    cat <<EOF
RedStars Helper — installeur / mise à jour.

Usage : install.sh [--appimage|--deb|--rpm] [--uninstall] [--force]

Sans option, le format est choisi automatiquement selon le système.
Relancer le script met simplement à jour vers la dernière version.
EOF
}

# --- options ---------------------------------------------------------------
FORMAT=""
ACTION="install"
FORCE=0
for arg in "$@"; do
    case "$arg" in
        --appimage) FORMAT=appimage ;;
        --deb)      FORMAT=deb ;;
        --rpm)      FORMAT=rpm ;;
        --uninstall) ACTION=uninstall ;;
        --force)    FORCE=1 ;;
        -h|--help)  usage; exit 0 ;;
        *) die "option inconnue : $arg (voir --help)" ;;
    esac
done

# --- outils réseau ---------------------------------------------------------
if command -v curl >/dev/null 2>&1; then
    fetch()    { curl -fsSL "$1"; }
    download() { curl -fSL --progress-bar -o "$2" "$1"; }
elif command -v wget >/dev/null 2>&1; then
    fetch()    { wget -qO- "$1"; }
    download() { wget -q --show-progress -O "$2" "$1"; }
else
    die "curl ou wget est requis."
fi

# --- architecture ----------------------------------------------------------
case "$(uname -m)" in
    x86_64|amd64)   ARCH_TOKENS="amd64 x86_64" ;;
    aarch64|arm64)  ARCH_TOKENS="aarch64 arm64" ;;
    *) die "architecture non supportée : $(uname -m)" ;;
esac

# --- détection du format ---------------------------------------------------
is_immutable() {
    command -v rpm-ostree >/dev/null 2>&1 && return 0
    grep -qE '[[:space:]]/usr[[:space:]][^[:space:]]+[[:space:]]ro[[:space:],]' \
        /proc/mounts 2>/dev/null && return 0
    return 1
}

detect_format() {
    if is_immutable; then echo appimage; return; fi
    if command -v apt-get >/dev/null 2>&1 || command -v dpkg >/dev/null 2>&1; then
        echo deb; return
    fi
    if command -v dnf >/dev/null 2>&1 || command -v zypper >/dev/null 2>&1 \
        || command -v rpm >/dev/null 2>&1; then
        echo rpm; return
    fi
    echo appimage
}

[ -n "$FORMAT" ] || FORMAT="$(detect_format)"

# --- résolution des assets de la dernière release --------------------------
release_json() {
    [ -n "${_JSON:-}" ] || _JSON="$(fetch "$API")" || die "impossible de joindre l'API GitHub."
    printf '%s' "$_JSON"
}

latest_version() {
    release_json | grep -o '"tag_name":[[:space:]]*"[^"]*"' \
        | sed 's/.*"\([^"]*\)"$/\1/' | head -n1
}

# pick_asset <ext> : URL du 1er asset .<ext> correspondant à notre architecture.
pick_asset() {
    ext="$1"
    urls="$(release_json | grep -o '"browser_download_url":[[:space:]]*"[^"]*"' \
        | sed 's/.*"\(https[^"]*\)"$/\1/' | grep -E "\.${ext}\$" || true)"
    for tok in $ARCH_TOKENS; do
        m="$(printf '%s\n' "$urls" | grep -E "[._-]${tok}[._-]|[._-]${tok}\." | head -n1 || true)"
        [ -n "$m" ] && { printf '%s' "$m"; return; }
    done
    # Repli : un seul asset de ce type, sans marqueur d'arch.
    printf '%s\n' "$urls" | head -n1
}

# --- lanceurs ~/.local/bin (mêmes que ceux posés par le binaire) -----------
write_launcher() { # <chemin> <contenu>
    path="$1"; contents="$2"
    if [ -e "$path" ] || [ -L "$path" ]; then
        # Ne jamais écraser un fichier étranger : on ne rafraîchit que les nôtres.
        if [ -f "$path" ] && grep -qF "$LAUNCHER_MARKER" "$path" 2>/dev/null; then
            :
        else
            warn "$path existe déjà et n'est pas un lanceur RedStars — laissé tel quel."
            return
        fi
    fi
    printf '%s\n' "$contents" > "$path"
    chmod 0755 "$path"
}

install_launchers() { # <cible>
    target="$1"
    mkdir -p "$BIN_DIR"
    write_launcher "$BIN_DIR/redhelper" \
        "#!/bin/sh
$LAUNCHER_MARKER
exec \"$target\" \"\$@\""
    write_launcher "$BIN_DIR/minitel" \
        "#!/bin/sh
$LAUNCHER_MARKER
exec \"$target\" minitel \"\$@\""
    case ":$PATH:" in
        *":$BIN_DIR:"*) : ;;
        *) warn "$BIN_DIR n'est pas dans votre PATH — ajoutez-le, ou ouvrez une nouvelle session." ;;
    esac
}

write_desktop_entry() { # <cible>
    target="$1"
    mkdir -p "$(dirname "$DESKTOP_FILE")"
    cat > "$DESKTOP_FILE" <<EOF
[Desktop Entry]
Type=Application
Name=Redstars Helper
Comment=Agent local RedStars (balance USB, WebGPU, démo)
Exec="$target" %u
Terminal=false
Categories=Development;Utility;
MimeType=x-scheme-handler/redstars-helper;
EOF
    command -v update-desktop-database >/dev/null 2>&1 \
        && update-desktop-database "$(dirname "$DESKTOP_FILE")" >/dev/null 2>&1 || true
}

# --- python3 (l'app en a besoin) -------------------------------------------
check_python() {
    command -v python3 >/dev/null 2>&1 && return
    warn "python3 introuvable — le helper en a besoin pour tourner. Installez-le :"
    warn "  Fedora: sudo dnf install python3   ·   Debian/Ubuntu: sudo apt install python3"
}

# --- installateurs ---------------------------------------------------------
sudo_run() {
    if [ "$(id -u)" -eq 0 ]; then "$@"; return; fi
    command -v sudo >/dev/null 2>&1 || die "sudo requis pour installer un paquet système."
    sudo "$@"
}

tmp_download() { # <url> -> imprime le chemin du fichier temporaire
    url="$1"
    tmp="$(mktemp "${TMPDIR:-/tmp}/redstars-helper.XXXXXX")"
    log "Téléchargement : $(basename "$url")"
    download "$url" "$tmp" >&2
    printf '%s' "$tmp"
}

install_appimage() {
    url="$(pick_asset AppImage)"
    [ -n "$url" ] || die "aucune AppImage $ARCH_TOKENS dans la release $VERSION."
    mkdir -p "$APPIMAGE_DIR"
    tmp="$APPIMAGE_PATH.download"
    log "Téléchargement : $(basename "$url")"
    download "$url" "$tmp"
    chmod +x "$tmp"
    mv -f "$tmp" "$APPIMAGE_PATH"
    log "Installé : $APPIMAGE_PATH"
    install_launchers "$APPIMAGE_PATH"
    write_desktop_entry "$APPIMAGE_PATH"
    check_python
    log "Fait. Dans un nouveau terminal :  redhelper console   (ou : minitel)"
    log "L'app apparaît aussi dans le menu (Redstars Helper) et démarre au login."
}

install_deb() {
    url="$(pick_asset deb)"
    [ -n "$url" ] || die "aucun .deb $ARCH_TOKENS dans la release $VERSION."
    tmp="$(tmp_download "$url")"
    log "Installation via le gestionnaire de paquets…"
    if command -v apt-get >/dev/null 2>&1; then
        sudo_run apt-get install -y "$tmp"
    else
        sudo_run dpkg -i "$tmp" || sudo_run apt-get -f install -y
    fi
    rm -f "$tmp"
    log "Fait.  redhelper console   (ou : minitel)"
}

install_rpm() {
    url="$(pick_asset rpm)"
    [ -n "$url" ] || die "aucun .rpm $ARCH_TOKENS dans la release $VERSION."
    tmp="$(tmp_download "$url")"
    log "Installation via le gestionnaire de paquets…"
    if command -v dnf >/dev/null 2>&1; then
        sudo_run dnf install -y "$tmp"
    elif command -v zypper >/dev/null 2>&1; then
        sudo_run zypper --non-interactive install --allow-unsigned-rpm "$tmp"
    else
        sudo_run rpm -Uvh "$tmp"
    fi
    rm -f "$tmp"
    log "Fait.  redhelper console   (ou : minitel)"
}

# --- désinstallation -------------------------------------------------------
remove_launcher() { # <chemin>
    [ -f "$1" ] && grep -qF "$LAUNCHER_MARKER" "$1" 2>/dev/null && rm -f "$1" && log "retiré : $1" || true
}

uninstall() {
    # Paquets système, si présents.
    if command -v dpkg >/dev/null 2>&1 && dpkg -s redstars-helper >/dev/null 2>&1; then
        log "Suppression du paquet .deb…"; sudo_run apt-get remove -y redstars-helper || true
    fi
    if command -v rpm >/dev/null 2>&1 && rpm -q redstars-helper >/dev/null 2>&1; then
        log "Suppression du paquet .rpm…"
        if command -v rpm-ostree >/dev/null 2>&1; then sudo_run rpm-ostree uninstall redstars-helper || true
        elif command -v dnf >/dev/null 2>&1; then sudo_run dnf remove -y redstars-helper || true
        else sudo_run rpm -e redstars-helper || true; fi
    fi
    # AppImage + fichiers utilisateur.
    [ -e "$APPIMAGE_PATH" ] && rm -f "$APPIMAGE_PATH" && log "retiré : $APPIMAGE_PATH" || true
    rmdir "$APPIMAGE_DIR" 2>/dev/null || true
    remove_launcher "$BIN_DIR/redhelper"
    remove_launcher "$BIN_DIR/minitel"
    [ -e "$DESKTOP_FILE" ] && rm -f "$DESKTOP_FILE" && log "retiré : $DESKTOP_FILE" || true
    log "Désinstallation terminée. (Le cache ~/.local/share/fr.redlinks.redstars-helper reste, au cas où.)"
}

# --- point d'entrée --------------------------------------------------------
if [ "$ACTION" = "uninstall" ]; then
    uninstall
    exit 0
fi

VERSION="$(latest_version)"
[ -n "$VERSION" ] || die "impossible de déterminer la dernière version (release absente ?)."
log "Dernière version : $VERSION · format : $FORMAT · arch : $(uname -m)"

case "$FORMAT" in
    appimage) install_appimage ;;
    deb)      install_deb ;;
    rpm)      install_rpm ;;
    *) die "format inconnu : $FORMAT" ;;
esac
