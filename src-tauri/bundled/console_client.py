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
    cursor = 0
    fd = sys.stdin.fileno()
    old = termios.tcgetattr(fd)
    try:
        tty.setraw(fd)
        while True:
            # Le serveur pagine tout seul : on lui donne le curseur, il place la
            # fenêtre. Le client n'a pas d'état de défilement à tenir — et donc pas
            # d'occasion de le désynchroniser.
            f = frame(app_url, token, app=app, org=org, role=role, slot=slot,
                      cursor=cursor, cols=cols, rows=rows, lang=lang)
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

            # Le curseur est un index de LIGNE, et toutes les lignes ne sont pas
            # focusables : un titre, une ligne vide, un en-tête de colonnes n'ont
            # rien à sélectionner. On se déplace donc de focusable en focusable —
            # et c'est la TRAME qui dit lesquels, pas nous.
            foc = f.get("focusables", [])
            here = next((i for i, x in enumerate(foc) if x["line"] == f.get("cursor")), 0)

            if k == "down" and foc:
                cursor = foc[min(here + 1, len(foc) - 1)]["line"]
            elif k == "up" and foc:
                cursor = foc[max(here - 1, 0)]["line"]
            elif k == "next" and foc:
                cursor = foc[min(here + max(1, rows - 5), len(foc) - 1)]["line"]
            elif k == "prev" and foc:
                cursor = foc[max(here - max(1, rows - 5), 0)]["line"]
            elif k == "enter":
                # Ce que fait Entrée est DIT PAR LA TRAME, pas décidé ici.
                if 0 <= here < len(foc):
                    act = foc[here].get("action") or {}
                    sys.stdout.write("\r\n\x1b[33m→ " + json.dumps(act, ensure_ascii=False) + "\x1b[0m\r\n")
                    sys.stdout.write("\x1b[2m(le rendu des modales n'est pas encore écrit — une touche)\x1b[0m")
                    sys.stdout.flush()
                    os.read(fd, 1)
            elif k in ("quit", "back"):
                return
    finally:
        termios.tcsetattr(fd, termios.TCSADRAIN, old)
        sys.stdout.write("\x1b[2J\x1b[H")
        sys.stdout.flush()


def run_accessible(app_url, token, app, org, role, slot, lang):
    """Le mode accessible. Une liste numérotée, on tape un numéro, on valide.

    Ce n'est PAS le mode visuel avec la voix par-dessus. Aucun mode raw, aucun
    curseur, aucune grille : le terminal reste en saisie ligne, ce qui est le
    seul régime où un lecteur d'écran est chez lui.

    Le curseur d'un mode visuel est justement ce qu'un lecteur d'écran suit le
    plus mal : il épelle les traits de tableau, annonce le remplissage des
    colonnes, et se perd à chaque repeint. Une liste numérotée n'a rien de tout
    ça — et c'est, accessoirement, l'interaction native de la machine qu'on
    imite : les services Minitel étaient des menus numérotés.
    """
    intro = "1"
    while True:
        url = f"{app_url}/api/console/frame/?" + urllib.parse.urlencode(
            dict(app=app, org=org, role=role, slot=slot, lang=lang, a11y=1, intro=intro))
        try:
            txt = _req(url, headers={"Authorization": f"Bearer {token}"}, raw=True).decode()
        except urllib.error.HTTPError as e:
            print(json.loads(e.read() or b"{}").get("error", f"Erreur HTTP {e.code}"))
            return
        print()
        print(txt)
        intro = "0"                       # ne relire le résumé qu'une fois

        try:
            answer = input("> ").strip().lower()
        except (EOFError, KeyboardInterrupt):
            return
        if answer in ("0", "q", ""):
            return
        if answer == "n":
            print("La création n'est pas encore écrite.")
            continue
        if answer.isdigit():
            # On redemande la trame pour lire l'action que le SERVEUR associe à
            # ce numéro. Le client ne décide de rien, même ici.
            f = frame(app_url, token, app=app, org=org, role=role, slot=slot, lang=lang)
            hit = next((x for x in f.get("focusables", []) if x["n"] == int(answer)), None)
            if not hit:
                print(f"Il n'y a pas d'élément numéro {answer}.")
                continue
            print(f"\n{hit['speech']}")
            print("(l'ouverture du détail n'est pas encore écrite)")
            continue
        print("Tapez un numéro, ou 0 pour revenir.")


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
    ap.add_argument("--accessible", action="store_true",
                    help="liste numérotée, texte pur, sans curseur — pour lecteur d'écran")
    a = ap.parse_args()

    # La taille du terminal ne doit JAMAIS être une condition d'existence :
    # get_terminal_size() lève dès que la sortie est redirigée — un pipe, un
    # cron, un lecteur d'écran. On retombe sur 80×24, la taille que tout le
    # monde a eue pendant quarante ans.
    def term_size():
        try:
            t = os.get_terminal_size()
            return t.columns, t.lines
        except OSError:
            return 80, 24

    tc, tr = term_size()
    cols, rows = (40, 24) if a.minitel else (a.cols or tc, a.rows or tr)

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

    if a.accessible:
        # Le sommaire aussi est une liste numérotée. Les slots que la console ne
        # sait PAS rendre (du React sur mesure : la carto d'eau, ses graphes) sont
        # annoncés comme tels au lieu d'être cachés — une dette qu'on dit tout
        # haut plutôt qu'un silence qui laisse croire que tout est là.
        rendable = [x for x in slots if x["kind"]]
        print(f"\n{app} version {s['version']}. Organisation {org['name']}.")
        print(f"{len(rendable)} rubriques.\n")
        for i, x in enumerate(rendable, 1):
            print(f"{i}. {x['id']}")
        muets = [x['id'] for x in slots if not x['kind']]
        if muets:
            print(f"\nNon disponibles en console : {', '.join(muets)}.")
        print(f"\nTapez un numéro de 1 à {len(rendable)} puis Entrée. 0 pour quitter.")
        try:
            n = input("> ").strip()
        except (EOFError, KeyboardInterrupt):
            return
        if not n.isdigit() or not (1 <= int(n) <= len(rendable)):
            return
        run_accessible(a.app_url, token, app, org["oid"], a.role, rendable[int(n)-1]["id"], a.lang)
        return

    choice = pick(slots, f"{app} v{s['version']} — {org['name']}",
                  lambda x: f"{x['id']:<16} {x['kind'] or '(React sur mesure — pas de rendu console)'}")
    if not choice or not choice["kind"]:
        return

    run(a.app_url, token, app, org["oid"], a.role, choice["id"], cols, rows, a.lang)


if __name__ == "__main__":
    main()
