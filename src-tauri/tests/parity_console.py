#!/usr/bin/env python3
"""Parité du client console : helper.py (Python, référence) ⇄ binaire Rust.

Les deux clients sont lancés dans un pseudo-terminal de taille fixe, reçoivent
la MÊME suite de touches, et l'on compare :
  1. les requêtes HTTP envoyées (méthode, URL + query, présence des en-têtes
     d'auth, corps) — côté Rust via REDSTARS_CONSOLE_TRACE, côté Python en
     interceptant urllib.request.urlopen ;
  2. les écrans affichés (le flux de sortie découpé sur l'effacement d'écran).

Isolation : HOME, XDG_STATE_HOME et XDG_RUNTIME_DIR sont des répertoires
temporaires — la vraie session (~/.local/state/redstars/session.json) et les
lanceurs (~/.local/bin/redhelper, que le binaire réécrit au démarrage) ne sont
jamais touchés. Aucun accès aux ports 49080/49443.

    python3 tests/parity_console.py --rust <binaire> --py bundled/helper.py \\
        [--scenario visual|minitel|accessible|all] [--out DIR]

Code de sortie 0 si tout est identique.
"""
import argparse
import difflib
import fcntl
import json
import os
import pty
import re
import select
import shutil
import struct
import subprocess
import sys
import tempfile
import termios
import time
import urllib.request

USER, PASSWORD = "user_test", "TestPassword1!"
API = os.environ.get("REDSTARS_API_URL", "https://api.dev.redstars.redlinks.fr")

UP, DOWN, LEFT = b"\x1b[A", b"\x1b[B", b"\x1b[D"   # ← = « Retour » dans les DEUX clients
ENTER = b"\r"


# ── Mode « wrapper » : le client Python, requêtes journalisées ────────────────

def py_wrapper(helper, argv):
    trace = os.environ.get("REDSTARS_CONSOLE_TRACE")
    orig = urllib.request.urlopen

    def logged(r, *a, **k):
        if isinstance(r, str):
            method, url, data, hdrs = "GET", r, None, {}
        else:
            method, url, data = r.get_method(), r.full_url, r.data
            hdrs = {k.lower(): v for k, v in r.header_items()}
        line = "%s %s auth=%s refresh=%s body=%s\n" % (
            method, url,
            "Bearer" if "authorization" in hdrs else "-",
            "y" if "x-refresh-token" in hdrs else "-",
            data.decode() if data else "-")
        if trace:
            with open(trace, "a") as f:
                f.write(line)
        return orig(r, *a, **k)

    urllib.request.urlopen = logged
    import runpy
    sys.argv = [helper] + argv
    try:
        runpy.run_path(helper, run_name="__main__")
    except SystemExit as e:
        raise


# ── Pilotage en pty ──────────────────────────────────────────────────────────

def run_in_pty(argv, env, steps, cols, rows, quiet=1.0, timeout=240):
    """Lance argv dans un pty cols×rows, joue `steps` (octets à taper), et rend
    tout ce qui a été affiché.

    Synchronisation : avant chaque frappe on attend que le client soit EN
    ATTENTE DE TOUCHE — sortie calme depuis `quiet` s ET aucune requête en vol
    (la dernière ligne de trace, écrite avant l'envoi, est plus ancienne que la
    dernière sortie). Certaines rubriques mettent plusieurs secondes à se rendre
    côté serveur : un simple délai fixe désynchroniserait les deux clients."""
    trace = env["REDSTARS_CONSOLE_TRACE"]
    pid, fd = pty.fork()
    if pid == 0:
        os.execvpe(argv[0], argv, env)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
    out = bytearray()
    st = {"out": time.time(), "trace": 0.0, "tsize": 0}

    def trace_poll():
        try:
            sz = os.path.getsize(trace)
        except OSError:
            sz = 0
        if sz != st["tsize"]:
            st["tsize"] = sz
            st["trace"] = time.time()

    def pump(deadline):
        while time.time() < deadline:
            r, _, _ = select.select([fd], [], [], 0.05)
            trace_poll()
            if r:
                try:
                    chunk = os.read(fd, 65536)
                except OSError:
                    return False
                if not chunk:
                    return False
                out.extend(chunk)
                st["out"] = time.time()
            elif time.time() - st["out"] >= quiet and st["out"] > st["trace"]:
                return True
        return True

    t0 = time.time()
    alive = pump(t0 + 30)
    for s in steps:
        if not alive or time.time() - t0 > timeout:
            break
        os.write(fd, s)
        st["out"] = time.time()          # la frappe elle-même compte comme activité
        alive = pump(time.time() + 30)
    # Laisser le processus finir.
    end = time.time() + 15
    status = -1
    while time.time() < end:
        done, stt = os.waitpid(pid, os.WNOHANG)
        if done:
            status = stt
            break
        pump(time.time() + 0.2)
    else:
        os.kill(pid, 9)
        os.waitpid(pid, 0)
    try:
        while True:
            r, _, _ = select.select([fd], [], [], 0.1)
            if not r:
                break
            chunk = os.read(fd, 65536)
            if not chunk:
                break
            out.extend(chunk)
    except OSError:
        pass
    os.close(fd)
    code = os.waitstatus_to_exitcode(status) if status != -1 else None
    return bytes(out), code


def login_session(user=USER):
    """Une session (access+refresh) obtenue une fois, partagée par les deux
    clients : même jeton ⇒ mêmes trames."""
    req = urllib.request.Request(
        f"{API}/api/v1/auth/login",
        data=json.dumps({"username": user, "password": PASSWORD}).encode(),
        headers={"Content-Type": "application/json"}, method="POST")
    with urllib.request.urlopen(req, timeout=30) as r:
        d = json.loads(r.read())
    return {"access_token": d["access_token"], "refresh_token": d["refresh_token"], "api": API}


def make_env(tmp, trace, seed):
    home = os.path.join(tmp, "home")
    state = os.path.join(tmp, "state")
    run = os.path.join(tmp, "run")
    for d in (home, state, run):
        os.makedirs(d, mode=0o700, exist_ok=True)
    if seed:
        os.makedirs(os.path.join(state, "redstars"), mode=0o700, exist_ok=True)
        with open(os.path.join(state, "redstars", "session.json"), "w") as f:
            json.dump(seed, f)
    env = {
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "HOME": home,
        "XDG_STATE_HOME": state,
        "XDG_RUNTIME_DIR": run,
        "TERM": "xterm-256color",
        "LANG": "C.UTF-8",
        "LC_ALL": "C.UTF-8",
        "TZ": os.environ.get("TZ", "Europe/Paris"),
        "REDSTARS_CONSOLE_TRACE": trace,
        "PYTHONIOENCODING": "utf-8",
    }
    # COLORTERM=truecolor ⇒ les deux clients annoncent img=1 et décodent les
    # images nn: — c'est ce qui exerce le décodeur (img_render ⇄ src/img).
    if os.environ.get("COLORTERM"):
        env["COLORTERM"] = os.environ["COLORTERM"]
    return env


SCENARIOS = {
    # Écran de login → dashboard → flèches → ouvrir une app → menu → rubrique →
    # bas, bas → Entrée (détail s'il y en a) → Retour → Retour → q.
    "visual": dict(
        args=[], seed=False, cols=100, rows=30,
        steps=[USER.encode(), ENTER, PASSWORD.encode(), ENTER,
               DOWN, DOWN, UP, ENTER,          # dashboard : 2e tuile
               DOWN, DOWN, ENTER,              # menu de l'app : 3e rubrique
               DOWN, DOWN, ENTER,              # rubrique : ouvrir une ligne (détail)
               LEFT,                           # ← Retour (détail → liste)
               LEFT,                           # ← Retour (liste → menu)
               LEFT,                           # ← Retour (menu → dashboard)
               b"x",                           # touche inconnue : la trame est redemandée
               b"q"]),
    # Session déjà ouverte (écrite au format Python, lue par les deux) → bouton
    # Continuer → Minitel 40×24 centré dans un 100×30.
    "minitel": dict(
        args=["--minitel"], seed=True, cols=100, rows=30,
        steps=[ENTER, DOWN, ENTER, ENTER, DOWN, LEFT, LEFT, b"q"]),
    # Mode accessible : listes numérotées, saisie ligne à ligne.
    "accessible": dict(
        args=["--accessible"], seed=True, cols=80, rows=24,
        steps=[b"1\r", b"1\r", b"1\r", b"\r", b"0\r"]),
    # App fournie par PLUSIEURS orgs (collab_test, tuile 1) : écran de choix
    # d'org rendu par le moteur, puis menu, puis Retour jusqu'à la sortie.
    "orgchoice": dict(
        args=[], seed=True, user="collab_test", cols=100, rows=30,
        steps=[ENTER, ENTER, DOWN, ENTER, ENTER, LEFT, LEFT, LEFT, b"q"]),
    "orgchoice-a11y": dict(
        args=["--accessible"], seed=True, user="collab_test", cols=80, rows=24,
        steps=[b"1\r", b"2\r", b"1\r", b"0\r"]),
    # --app épinglé : Retour au choix d'org = sortie (rien au-dessus).
    "pinned": dict(
        args=["--app", "cellule", "--lang", "en"], seed=True, user="collab_test",
        cols=90, rows=26, steps=[ENTER, LEFT]),
    # Jeton d'accès périmé + refresh valide : renouvellement silencieux
    # (POST /auth/refresh, X-Refresh-Token) puis session réécrite.
    "refresh": dict(
        args=["--accessible"], seed="expired", cols=80, rows=24, steps=[b"0\r"]),
    # Mauvais mot de passe en ligne de commande : « Connexion refusée », code 1.
    "badlogin": dict(
        args=["--user", USER, "--password", "faux"], seed=False, cols=80, rows=24, steps=[]),
}

EXPIRED_JWT = "eyJhbGciOiJIUzI1NiJ9.eyJleHAiOjEwMDB9.x"   # {"exp": 1000}


def normalize(b):
    s = b.decode("utf-8", "replace")
    return s


def screens(s):
    return [x for x in s.split("\x1b[2J")]


def main():
    if len(sys.argv) > 2 and sys.argv[1] == "--py-wrapper":
        return py_wrapper(sys.argv[2], sys.argv[3:])

    ap = argparse.ArgumentParser()
    ap.add_argument("--rust", required=True)
    ap.add_argument("--py", required=True)
    ap.add_argument("--scenario", default="all")
    ap.add_argument("--out", default=None)
    a = ap.parse_args()

    names = list(SCENARIOS) if a.scenario == "all" else a.scenario.split(",")
    out_dir = a.out or tempfile.mkdtemp(prefix="parity-console-")
    os.makedirs(out_dir, exist_ok=True)
    seeds = {}
    ok_all = True
    for name in names:
        sc = SCENARIOS[name]
        user = sc.get("user", USER)
        if sc["seed"] is True and user not in seeds:
            seeds[user] = login_session(user)
        res = {}
        for impl in ("py", "rust"):
            tmp = tempfile.mkdtemp(prefix=f"pc-{name}-{impl}-", dir=out_dir)
            trace = os.path.join(tmp, "trace.log")
            if sc["seed"] == "expired":
                # Chaque client son propre refresh : le serveur peut le faire tourner.
                seed = dict(login_session(user), access_token=EXPIRED_JWT)
            else:
                seed = seeds.get(user) if sc["seed"] else None
            env = make_env(tmp, trace, seed)
            if impl == "py":
                argv = [sys.executable, "-I", os.path.abspath(__file__), "--py-wrapper",
                        os.path.abspath(a.py), "console"] + sc["args"]
            else:
                argv = [os.path.abspath(a.rust), "console"] + sc["args"]
            out, code = run_in_pty(argv, env, sc["steps"], sc["cols"], sc["rows"])
            with open(os.path.join(out_dir, f"{name}.{impl}.out"), "wb") as f:
                f.write(out)
            tr = open(trace).read() if os.path.exists(trace) else ""
            with open(os.path.join(out_dir, f"{name}.{impl}.trace"), "w") as f:
                f.write(tr)
            sess = os.path.join(tmp, "state", "redstars", "session.json")
            sd = json.load(open(sess)) if os.path.exists(sess) else None
            if sd and seed and sc["seed"] == "expired":
                print(f"   [{impl}] session renouvelée : access changé={sd['access_token'] != EXPIRED_JWT}")
            res[impl] = dict(out=normalize(out), trace=tr, code=code, session=sd)

        p, r = res["py"], res["rust"]
        same_trace = p["trace"] == r["trace"]
        same_out = p["out"] == r["out"]
        print(f"── {name}: requêtes {'IDENTIQUES' if same_trace else 'DIFFÉRENTES'} "
              f"({p['trace'].count(chr(10))} / {r['trace'].count(chr(10))}), "
              f"sortie {'IDENTIQUE' if same_out else 'DIFFÉRENTE'} "
              f"({len(p['out'])} / {len(r['out'])} car.), code {p['code']} / {r['code']}")
        if p["session"] or r["session"]:
            ks = lambda d: sorted(d) if d else None
            print(f"   session.json clés : py={ks(p['session'])} rust={ks(r['session'])}")
        if not same_trace:
            ok_all = False
            for ln in difflib.unified_diff(p["trace"].splitlines(), r["trace"].splitlines(),
                                           "python", "rust", lineterm="", n=1):
                print("   " + ln[:300])
        if not same_out:
            ok_all = False
            ps, rs = screens(p["out"]), screens(r["out"])
            print(f"   écrans : {len(ps)} / {len(rs)}")
            for i, (x, y) in enumerate(zip(ps, rs)):
                if x != y:
                    print(f"   1er écran différent : #{i}")
                    for ln in difflib.unified_diff(x.split("\r\n"), y.split("\r\n"),
                                                   "python", "rust", lineterm="", n=0):
                        print("   " + repr(ln)[:300])
                    break
    print(f"\nsorties brutes : {out_dir}")
    return 0 if ok_all else 1


if __name__ == "__main__":
    sys.exit(main())
