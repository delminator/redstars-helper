#!/usr/bin/env python3
"""RedStars en console — le client terminal.

    python3 console_client.py --user admin_test --app eau

Ce client est DÉLIBÉRÉMENT BÊTE. Il ne sait pas ce qu'est une app, un slot, ni
une colonne. Il envoie « je suis un terminal de N colonnes, je suis ici, on a
appuyé sur ↓ » et il affiche les octets qu'on lui renvoie.

Tout le savoir — quelles colonnes, quel format, ce que fait Entrée — vit dans le
core, à côté des renderers React qui lisent EXACTEMENT les mêmes descripteurs.
Une app déployée demain marche ici sans qu'on écrive une ligne. Le jour où ce
fichier contient un `if app == 'eau'`, l'architecture a échoué.

Il parle deux protocoles :
  --mode ansi      un terminal moderne (défaut)
  --mode videotex  un Minitel : 40×24, 8 couleurs, trames diffées

Le Minitel n'est pas un gag. 1200 bauds, c'est 120 caractères par seconde : un
repaint complet de 40×24 prend ~9 s, un diff après une flèche en prend 0,8. La
contrainte force une hiérarchie d'information honnête — et un slot lisible sur
un Minitel est lisible partout.
"""
import argparse
import getpass
import json
import os
import ssl
import sys
import termios
import tty
import urllib.error
import urllib.parse
import urllib.request

DEFAULT_APP_URL = "https://dev.redstars.redlinks.fr"
DEFAULT_API_URL = "https://api.dev.redstars.redlinks.fr"

_CTX = ssl.create_default_context()


def _req(url, data=None, headers=None, method=None, raw=False):
    body = json.dumps(data).encode() if data is not None else None
    h = {"Content-Type": "application/json", **(headers or {})}
    r = urllib.request.Request(url, data=body, headers=h, method=method or ("POST" if body else "GET"))
    with urllib.request.urlopen(r, context=_CTX, timeout=30) as resp:
        payload = resp.read()
    return payload if raw else json.loads(payload or b"{}")


# ── Auth + org ──────────────────────────────────────────────────────────────

def login(api, username, password):
    d = _req(f"{api}/api/v1/auth/login", {"username": username, "password": password})
    return d["access_token"]


def orgs(api, token):
    d = _req(f"{api}/api/v1/organizations", headers={"Authorization": f"Bearer {token}"})
    return d if isinstance(d, list) else d.get("organizations", d.get("data", []))


# ── Le moteur : on ne fait que DEMANDER une trame ───────────────────────────

def frame(app_url, token, **q):
    url = f"{app_url}/api/console/frame/?" + urllib.parse.urlencode(q)
    try:
        return _req(url, headers={"Authorization": f"Bearer {token}"})
    except urllib.error.HTTPError as e:
        return {"error": json.loads(e.read() or b"{}").get("error", f"HTTP {e.code}")}


# ── Clavier ─────────────────────────────────────────────────────────────────

def read_key(fd):
    """Une touche → une intention. Les mêmes intentions que la trame annonce
    dans ses `hints`, donc la même sémantique qu'un ENVOI/SUITE de Minitel."""
    c = os.read(fd, 1)
    if c == b"\x1b":                       # séquence CSI, ou Échap seul
        seq = os.read(fd, 2)
        return {b"[A": "up", b"[B": "down", b"[5": "prev", b"[6": "next"}.get(seq, "back")
    return {b"\r": "enter", b"\n": "enter", b"q": "quit", b"n": "new"}.get(c, None)


def pick(items, label, render):
    """Un menu minimal, avec les mêmes flèches. Sert au choix de l'org et de
    l'app — c'est-à-dire la seule navigation que le client s'autorise à
    connaître, parce qu'elle précède toute notion d'app."""
    i = 0
    fd = sys.stdin.fileno()
    old = termios.tcgetattr(fd)
    try:
        tty.setraw(fd)
        while True:
            sys.stdout.write("\x1b[2J\x1b[H\x1b[1;36m" + label + "\x1b[0m\r\n\r\n")
            for k, it in enumerate(items):
                mark = "\x1b[7m" if k == i else ""
                sys.stdout.write(f"  {mark}{render(it):<50}\x1b[0m\r\n")
            sys.stdout.write("\r\n\x1b[2m↑ ↓ pour choisir · ↵ valider · q quitter\x1b[0m\r\n")
            sys.stdout.flush()
            k = read_key(fd)
            if k == "up":
                i = (i - 1) % len(items)
            elif k == "down":
                i = (i + 1) % len(items)
            elif k == "enter":
                return items[i]
            elif k in ("quit", "back"):
                return None
    finally:
        termios.tcsetattr(fd, termios.TCSADRAIN, old)


# ── La boucle ───────────────────────────────────────────────────────────────

def run(app_url, token, app, org, role, slot, cols, rows, lang):
    cursor, offset = 0, 0
    fd = sys.stdin.fileno()
    old = termios.tcgetattr(fd)
    try:
        tty.setraw(fd)
        while True:
            f = frame(app_url, token, app=app, org=org, role=role, slot=slot,
                      cursor=cursor, offset=offset, cols=cols, rows=rows, lang=lang)
            if "error" in f:
                sys.stdout.write("\x1b[2J\x1b[H\x1b[31m" + f["error"].replace("\n", "\r\n") + "\x1b[0m\r\n\r\n")
                sys.stdout.write("\x1b[2mUne touche pour revenir…\x1b[0m")
                sys.stdout.flush()
                os.read(fd, 1)
                return

            # Le serveur a déjà tout composé. On imprime.
            sys.stdout.write(f["ansi"].replace("\n", "\r\n"))
            sys.stdout.flush()

            k = read_key(fd)
            n = f.get("rows", 0)
            body = max(1, rows - 5)
            if k == "down":
                cursor = min(cursor + 1, max(0, n - 1))
                if cursor >= offset + body:
                    offset += 1
            elif k == "up":
                cursor = max(cursor - 1, 0)
                if cursor < offset:
                    offset = max(0, offset - 1)
            elif k == "next":
                cursor = min(cursor + body, max(0, n - 1)); offset = min(offset + body, max(0, n - body))
            elif k == "prev":
                cursor = max(cursor - body, 0); offset = max(offset - body, 0)
            elif k == "enter":
                # Ce que fait Entrée est DIT PAR LA TRAME, pas décidé ici.
                foc = f.get("focusables", [])
                cur = f.get("cursor", -1)
                if 0 <= cur < len(foc):
                    act = foc[cur].get("action") or {}
                    sys.stdout.write("\r\n\x1b[33m→ " + json.dumps(act) + "\x1b[0m\r\n")
                    sys.stdout.write("\x1b[2m(le rendu des modales n'est pas encore écrit — une touche)\x1b[0m")
                    sys.stdout.flush()
                    os.read(fd, 1)
            elif k in ("quit", "back"):
                return
    finally:
        termios.tcsetattr(fd, termios.TCSADRAIN, old)
        sys.stdout.write("\x1b[2J\x1b[H")
        sys.stdout.flush()


def main():
    ap = argparse.ArgumentParser(description="RedStars en console")
    ap.add_argument("--app-url", default=os.environ.get("REDSTARS_APP_URL", DEFAULT_APP_URL))
    ap.add_argument("--api-url", default=os.environ.get("REDSTARS_API_URL", DEFAULT_API_URL))
    ap.add_argument("--user")
    ap.add_argument("--password")
    ap.add_argument("--app", help="id de l'app (eau, ludo…). Omis → on demande.")
    ap.add_argument("--role", default="collaborator")
    ap.add_argument("--lang", default="fr")
    ap.add_argument("--cols", type=int, default=0, help="0 = taille réelle du terminal")
    ap.add_argument("--rows", type=int, default=0)
    ap.add_argument("--minitel", action="store_true", help="force 40×24, comme un Minitel")
    a = ap.parse_args()

    cols, rows = (40, 24) if a.minitel else (
        a.cols or os.get_terminal_size().columns,
        a.rows or os.get_terminal_size().lines,
    )

    user = a.user or input("Utilisateur : ")
    pw = a.password or getpass.getpass("Mot de passe : ")
    try:
        token = login(a.api_url, user, pw)
    except urllib.error.HTTPError as e:
        sys.exit(f"Connexion refusée (HTTP {e.code}).")

    o = orgs(a.api_url, token)
    if not o:
        sys.exit("Aucune organisation.")
    org = o[0] if len(o) == 1 else pick(o, "Organisation", lambda x: x["name"])
    if not org:
        return

    app = a.app
    if not app:
        app = input("App (eau, ludo…) : ").strip()

    # Le sommaire — et le recensement honnête : les slots `kind: null` sont du
    # React sur mesure, sans forme console. On les affiche barrés plutôt que de
    # les cacher : c'est une dette visible, pas un secret.
    s = frame(a.app_url, token, app=app, org=org["oid"], role=a.role)
    if "error" in s:
        sys.exit(s["error"])
    slots = s["slots"]
    choice = pick(slots, f"{app} v{s['version']} — {org['name']}",
                  lambda x: f"{x['id']:<16} {x['kind'] or '(React sur mesure — pas de rendu console)'}")
    if not choice or not choice["kind"]:
        return

    run(a.app_url, token, app, org["oid"], a.role, choice["id"], cols, rows, a.lang)


if __name__ == "__main__":
    main()
