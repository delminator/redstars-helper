#!/usr/bin/env python3
"""Parité HTTP entre helper.py (référence) et le helper natif Rust.

    python3 -I tests/parity_http.py <binaire redstars-helper> [--keep]

Lance les DEUX serveurs sur des ports décalés (jamais 49080/49443 : un vrai
helper tourne peut-être sur la machine), chacun avec un HOME factice (profil
Firefox jetable, caches refs/iso isolés), joue la même suite de requêtes sur
chacun et compare statut + en-têtes + corps, après normalisation de ce qui est
volatil (HOME, identifiants, version, horodatages, espace disque libre).

Ne lance jamais d'application (refs/open : seulement les cas 403/404), ne monte
d'ISO que si `udisksctl` marche sans interaction (sinon : ignoré, et dit).
"""
import http.client
import json
import os
import re
import shutil
import signal
import socket
import ssl
import subprocess
import sys
import tempfile
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
BUNDLED = HERE.parent / 'bundled'
PY_PORT, PY_TLS = 59080, 59443
RS_PORT, RS_TLS = 59180, 59543
ALLOWED = 'https://dev.redstars.redlinks.fr'
EVIL = 'https://evil.example'

# Différences voulues (cf. rapport de portage) : on les montre, on ne les compte pas.
EXPECTED = {
    'GET /helper/status': 'version = celle du shell (1.x)',
    'POST /helper/update': 'plus de script à mettre à jour : réponse fixe, crochet du shell',
    'POST /helper/session ok': 'console::session::save est encore un bouchon (unimplemented!) → 500',
    'POST /helper/console ok': 'console::spawn_terminal est encore un bouchon → 500',
    'GET /helper/scale': 'pyserial présent côté Python → message SerialException ; natif = message « not present »',
}


def wait_port(port, timeout=15):
    t0 = time.time()
    while time.time() - t0 < timeout:
        try:
            socket.create_connection(('127.0.0.1', port), 0.2).close()
            return True
        except OSError:
            time.sleep(0.1)
    return False


def make_home(root: Path):
    ff = root / '.mozilla' / 'firefox'
    prof = ff / 'abcd.default-release'
    prof.mkdir(parents=True)
    (ff / 'profiles.ini').write_text(
        '[Profile0]\nName=default\nIsRelative=1\nPath=zzzz.default\nDefault=1\n\n'
        '[Install4F96D1932A9F858E]\nDefault=abcd.default-release\nLocked=1\n\n'
        '[General]\nStartWithLastProfile=1\nVersion=2\n')
    (prof / 'user.js').write_text('// mes prefs\nuser_pref("a.b", 1);')
    (prof / 'prefs.js').write_text('user_pref("x", 1);\r\nuser_pref("dom.webgpu.enabled", true);\n')
    return root


def request(port, method, path, origin=None, body=None, raw_body=None, headers=None, chunked=False):
    c = http.client.HTTPConnection('127.0.0.1', port, timeout=30)
    h = dict(headers or {})
    if origin:
        h['Origin'] = origin
    data = None
    if raw_body is not None:
        data = raw_body
    elif body is not None:
        data = json.dumps(body).encode()
        h['Content-Type'] = 'application/json'
    if chunked:
        c.putrequest(method, path, skip_accept_encoding=True)
        for k, v in h.items():
            c.putheader(k, v)
        c.putheader('Transfer-Encoding', 'chunked')
        c.endheaders()
        c.send(b'%x\r\n%s\r\n0\r\n\r\n' % (len(data), data))
    else:
        c.request(method, path, body=data, headers=h)
    r = c.getresponse()
    out = (r.status, [(k, v) for k, v in r.getheaders()], r.read())
    c.close()
    return out


IGNORED_HEADERS = {'date', 'server', 'content-length', 'connection', 'keep-alive'}


def norm(side, status, headers, body, ctx):
    hs = sorted((k.lower(), v) for k, v in headers if k.lower() not in IGNORED_HEADERS)
    text = body.decode('utf-8', 'replace')
    for a, b in ctx['subst']:
        text = text.replace(a, b)
    text = re.sub(r'"version": "[^"]*"', '"version": "<V>"', text)
    text = re.sub(r'"(updated_at|age_ms|free|used)": [-0-9.e+]+', r'"\1": <N>', text)
    text = re.sub(r'\b[0-9a-f]{12}\b', '<ID>', text)
    return status, hs, text


def run_suite(port, home: Path, files: Path, iso_ok: bool):
    """Joue la suite ; rend [(nom, (status, headers, body))]."""
    res = []

    def hit(name, *a, **kw):
        r = request(port, *a, **kw)
        res.append((name, r))
        return r

    def j(r):
        try:
            return json.loads(r[2])
        except Exception:
            return {}

    # ── GET / HEAD / OPTIONS / méthodes inconnues ─────────────────────────────
    hit('GET /helper/status', 'GET', '/helper/status')
    hit('GET /helper/ping', 'GET', '/helper/ping')
    hit('GET /helper/ping allowed', 'GET', '/helper/ping', origin=ALLOWED)
    hit('GET /helper/ping evil', 'GET', '/helper/ping', origin=EVIL)
    hit('GET /helper/ping legacy', 'GET', '/helper/ping', origin='http://localhost:9999')
    hit('GET /', 'GET', '/')
    hit('GET / allowed', 'GET', '/', origin=ALLOWED)
    hit('GET /cert.pem', 'GET', '/cert.pem')
    hit('GET /helper', 'GET', '/helper')
    hit('GET /helper/', 'GET', '/helper/')
    hit('GET /helper/nope', 'GET', '/helper/nope')
    hit('GET /helper/status?x=1', 'GET', '/helper/status?x=1#f')
    hit('GET //helper/ping', 'GET', '//helper/ping')
    hit('GET /helper/st%61tus', 'GET', '/helper/st%61tus')
    hit('GET /helper/status/', 'GET', '/helper/status/')
    hit('HEAD /helper/status', 'HEAD', '/helper/status')
    hit('HEAD /helper/whatever allowed', 'HEAD', '/helper/whatever', origin=ALLOWED)
    hit('HEAD /', 'HEAD', '/')
    hit('HEAD / allowed', 'HEAD', '/', origin=ALLOWED)
    hit('OPTIONS /helper/scale preflight', 'OPTIONS', '/helper/scale', origin=ALLOWED,
        headers={'Access-Control-Request-Method': 'GET',
                 'Access-Control-Request-Private-Network': 'true'})
    hit('OPTIONS /helper/console evil', 'OPTIONS', '/helper/console', origin=EVIL)
    hit('OPTIONS /', 'OPTIONS', '/')
    hit('OPTIONS / allowed', 'OPTIONS', '/', origin=ALLOWED)
    hit('PUT /helper/status', 'PUT', '/helper/status', raw_body=b'')
    hit('DELETE / allowed', 'DELETE', '/', origin=ALLOWED)
    hit('PATCH /helper/x', 'PATCH', '/helper/x', raw_body=b'{}')

    # ── matériel / disque ─────────────────────────────────────────────────────
    hit('GET /helper/disk', 'GET', '/helper/disk')
    hit('GET /helper/disk?path=sub', 'GET', '/helper/disk?path=' + str(home / 'disk' / 'a%20b+c'))
    hit('GET /helper/disk?path=file', 'GET', '/helper/disk?path=' + str(files / 'a.txt'))
    hit('GET /helper/disk?path=/proc', 'GET', '/helper/disk?path=/proc/nope/x')
    hit('GET /helper/lsusb', 'GET', '/helper/lsusb')
    hit('GET /helper/scale', 'GET', '/helper/scale')

    # ── POST générique ────────────────────────────────────────────────────────
    hit('POST /', 'POST', '/', body={})
    hit('POST /x allowed', 'POST', '/x', origin=ALLOWED, body={})
    hit('POST /helper/nope', 'POST', '/helper/nope', body={})

    # ── session / console : contrôle d'origine ────────────────────────────────
    hit('POST /helper/session no-origin', 'POST', '/helper/session', body={'token': 't', 'refresh': 'r'})
    hit('POST /helper/session evil', 'POST', '/helper/session', origin=EVIL, body={'token': 't', 'refresh': 'r'})
    hit('POST /helper/session bad-json', 'POST', '/helper/session', origin=ALLOWED, raw_body=b'{nope')
    hit('POST /helper/session token-only', 'POST', '/helper/session', origin=ALLOWED, body={'token': 't'})
    hit('POST /helper/session blank', 'POST', '/helper/session', origin=ALLOWED, body={'token': '  ', 'refresh': 'r'})
    hit('POST /helper/session list', 'POST', '/helper/session', origin=ALLOWED, body=[1])
    hit('POST /helper/session int-token', 'POST', '/helper/session', origin=ALLOWED, body={'token': 5, 'refresh': 'r'})
    hit('POST /helper/session ok', 'POST', '/helper/session', origin=ALLOWED,
        body={'token': 'tok', 'refresh': 'ref', 'api_url': 'http://127.0.0.1:9'})
    hit('POST /helper/console no-origin', 'POST', '/helper/console', body={'token': 't'})
    hit('POST /helper/console evil', 'POST', '/helper/console', origin=EVIL, body={'token': 't'})
    hit('POST /helper/console empty', 'POST', '/helper/console', origin=ALLOWED, body={})
    hit('POST /helper/console chunked', 'POST', '/helper/console', origin=ALLOWED,
        raw_body=b'{"token": "t"}', chunked=True)
    hit('POST /helper/console ok', 'POST', '/helper/console', origin=ALLOWED,
        body={'token': 'tok', 'app': 'eau'}, headers={'X-Dry': '1'}) if os.environ.get('PARITY_CONSOLE') else None

    # ── refs ──────────────────────────────────────────────────────────────────
    hit('GET /helper/refs/list unknown', 'GET', '/helper/refs/list?id=nope')
    hit('GET /helper/refs/list none', 'GET', '/helper/refs/list')
    hit('POST /helper/refs/mount empty', 'POST', '/helper/refs/mount', body={})
    hit('POST /helper/refs/mount bad-json', 'POST', '/helper/refs/mount', raw_body=b'not json')
    hit('POST /helper/refs/mount missing', 'POST', '/helper/refs/mount',
        body={'paths': [str(files / 'a.txt'), '/nonexistent/zz']})
    r = hit('POST /helper/refs/mount ok', 'POST', '/helper/refs/mount', body={
        'label': ' TEST ',
        'paths': [str(files / 'a.txt'), str(files / 'dir1'), str(files / 'other' / 'a.txt'),
                  str(files / 'x.'), str(files / 'noext'), str(files / 'link-to-b')]})
    rid = j(r).get('id', '')
    mp = j(r).get('mount_path', '')
    hit('GET /helper/refs/list', 'GET', '/helper/refs/list?id=' + rid)
    hit('POST /helper/refs/add', 'POST', '/helper/refs/add',
        body={'id': rid, 'paths': [str(files / 'b.txt'), '/nonexistent/z.txt', str(files / 'a.txt')]})
    hit('POST /helper/refs/add unknown', 'POST', '/helper/refs/add', body={'id': 'nope', 'paths': ['/']})
    hit('POST /helper/refs/add nopaths', 'POST', '/helper/refs/add', body={'id': rid})
    hit('GET /helper/refs/list after add', 'GET', '/helper/refs/list?id=' + rid)
    hit('POST /helper/refs/open outside', 'POST', '/helper/refs/open', body={'id': rid, 'path': '/etc/passwd'})
    hit('POST /helper/refs/open dotdot', 'POST', '/helper/refs/open', body={'id': rid, 'path': mp + '/../../x'})
    hit('POST /helper/refs/open prefix', 'POST', '/helper/refs/open', body={'id': rid, 'path': mp + 'x/y'})
    hit('POST /helper/refs/open unknown', 'POST', '/helper/refs/open', body={'id': 'nope'})
    hit('POST /helper/refs/mount label-blank', 'POST', '/helper/refs/mount',
        body={'label': '   ', 'paths': [str(files / 'b.txt')]})
    hit('POST /helper/refs/unmount', 'POST', '/helper/refs/unmount', body={'id': rid})
    hit('POST /helper/refs/unmount again', 'POST', '/helper/refs/unmount', body={'id': rid})
    hit('POST /helper/refs/unmount none', 'POST', '/helper/refs/unmount', body={})

    # ── iso ───────────────────────────────────────────────────────────────────
    hit('GET /helper/iso/list unknown', 'GET', '/helper/iso/list?id=nope')
    hit('POST /helper/iso/mount bad-b64-1', 'POST', '/helper/iso/mount', body={'payload': 'a'})
    hit('POST /helper/iso/mount bad-pad', 'POST', '/helper/iso/mount', body={'payload': 'aGVsbG8'})
    hit('POST /helper/iso/mount bad-files', 'POST', '/helper/iso/mount', body={'files': {'x': 'abcde'}})
    hit('POST /helper/iso/mount files-int', 'POST', '/helper/iso/mount', body={'files': {'x': 5}})
    hit('POST /helper/iso/mount non-ascii', 'POST', '/helper/iso/mount', body={'payload': 'é'})
    hit('POST /helper/iso/unmount unknown', 'POST', '/helper/iso/unmount', body={'id': 'nope'})
    hit('POST /helper/iso/open outside', 'POST', '/helper/iso/open', body={'path': '/etc'})
    hit('POST /helper/iso/open none', 'POST', '/helper/iso/open', body={})
    if iso_ok:
        r = hit('POST /helper/iso/mount ok', 'POST', '/helper/iso/mount',
                body={'label': 'PARITY', 'files': {'../evil/hello.txt': 'aGVsbG8=', 'b.bin': 'AAE='}})
        iid = j(r).get('id', '')
        hit('GET /helper/iso/list', 'GET', '/helper/iso/list?id=' + iid)
        hit('POST /helper/refs/open iso-fallback outside', 'POST', '/helper/refs/open',
            body={'id': iid, 'path': '/etc'})
        hit('POST /helper/iso/unmount', 'POST', '/helper/iso/unmount', body={'id': iid})

    # ── webgpu (HOME factice) ─────────────────────────────────────────────────
    hit('POST /helper/reset-webgpu 1', 'POST', '/helper/reset-webgpu', body={})
    hit('POST /helper/enable-webgpu 1', 'POST', '/helper/enable-webgpu', body={})
    hit('POST /helper/enable-webgpu 2', 'POST', '/helper/enable-webgpu', body={})
    prof = home / '.mozilla' / 'firefox' / 'abcd.default-release'
    res.append(('FILE user.js after enable', (0, [], (prof / 'user.js').read_bytes())))
    hit('POST /helper/reset-webgpu 2', 'POST', '/helper/reset-webgpu', body={})
    res.append(('FILE user.js after reset', (0, [], (prof / 'user.js').read_bytes()
                                             if (prof / 'user.js').exists() else b'<absent>')))
    res.append(('FILE prefs.js after reset', (0, [], (prof / 'prefs.js').read_bytes())))
    (home / '.mozilla' / 'firefox' / 'profiles.ini').unlink()
    hit('POST /helper/enable-webgpu no-profile', 'POST', '/helper/enable-webgpu', body={})

    # ── update (Python : api_base injoignable pour échouer vite) ──────────────
    hit('POST /helper/update', 'POST', '/helper/update', body={'api_base': 'http://127.0.0.1:9'})
    return [x for x in res if x is not None]


def https_status(port):
    ctx = ssl.create_default_context()
    raw = socket.create_connection(('127.0.0.1', port), 5)
    s = ctx.wrap_socket(raw, server_hostname='local.redlinks.fr')
    s.sendall(b'GET /helper/ping HTTP/1.1\r\nHost: local.redlinks.fr\r\nOrigin: ' + ALLOWED.encode()
              + b'\r\nConnection: close\r\n\r\n')
    data = b''
    while True:
        chunk = s.recv(65536)
        if not chunk:
            break
        data += chunk
    s.close()
    head, _, body = data.partition(b'\r\n\r\n')
    return head.split(b'\r\n')[0].decode(), body.decode()


def udisks_ok():
    if not shutil.which('udisksctl') or not shutil.which('xorrisofs'):
        return False
    try:
        r = subprocess.run(['udisksctl', 'status'], capture_output=True, timeout=5)
        return r.returncode == 0
    except Exception:
        return False


def main():
    if len(sys.argv) < 2:
        print(__doc__)
        return 2
    rust_bin = os.path.abspath(sys.argv[1])
    work = Path(tempfile.mkdtemp(prefix='parity-'))
    pydir = work / 'py'
    pydir.mkdir()
    for f in ('helper.py', 'cert.pem', 'key.pem'):
        shutil.copy(BUNDLED / f, pydir / f)  # COPIE : l'auto-update réécrit le fichier lancé
    files = work / 'files'
    (files / 'dir1').mkdir(parents=True)
    (files / 'other').mkdir()
    (files / 'a.txt').write_text('aaa')
    (files / 'b.txt').write_text('bbbbbb')
    (files / 'other' / 'a.txt').write_text('autre a')
    (files / 'x.').write_text('point final')
    (files / 'noext').write_text('n')
    (files / 'dir1' / 'f').write_text('f')
    os.symlink(files / 'b.txt', files / 'link-to-b')
    homes = {'py': make_home(work / 'home-py'), 'rs': make_home(work / 'home-rs')}

    base_env = {k: v for k, v in os.environ.items()
                if k not in ('XDG_CACHE_HOME', 'XDG_DATA_HOME', 'REDSTARS_HELPER_CERT_DIR', 'REDSTARS_SCALE_FILE')}
    base_env['REDSTARS_HELPER_IDLE_EXIT_S'] = '0'
    procs = []
    py_env = dict(base_env, HOME=str(homes['py']), HELPER_PORT=str(PY_PORT), HELPER_HTTPS_PORT=str(PY_TLS),
                  REDSTARS_API_BASE='http://127.0.0.1:9')
    rs_env = dict(base_env, HOME=str(homes['rs']), HELPER_PORT=str(RS_PORT), HELPER_HTTPS_PORT=str(RS_TLS))
    log_py = open(work / 'py.log', 'w')
    log_rs = open(work / 'rs.log', 'w')
    procs.append(subprocess.Popen([sys.executable, '-I', 'helper.py'], cwd=pydir, env=py_env,
                                  stdout=log_py, stderr=subprocess.STDOUT, start_new_session=True))
    procs.append(subprocess.Popen([rust_bin, 'serve'], cwd=work, env=rs_env,
                                  stdout=log_rs, stderr=subprocess.STDOUT, start_new_session=True))
    try:
        for p in (PY_PORT, PY_TLS, RS_PORT, RS_TLS):
            if not wait_port(p):
                print(f'port {p} jamais ouvert — voir {work}/*.log')
                return 1
        iso_ok = udisks_ok()
        out = {}
        for side, port in (('py', PY_PORT), ('rs', RS_PORT)):
            out[side] = run_suite(port, homes[side], files, iso_ok)
        same = diff = expected = 0
        for (name, a), (name2, b) in zip(out['py'], out['rs']):
            assert name == name2
            ctx_a = {'subst': [(str(homes['py']), '<HOME>')]}
            ctx_b = {'subst': [(str(homes['rs']), '<HOME>')]}
            na, nb = norm('py', *a, ctx_a), norm('rs', *b, ctx_b)
            if na == nb:
                same += 1
                print(f'  =  {name}')
                continue
            tag = 'EXPECTED' if name in EXPECTED else 'DIFF'
            if tag == 'EXPECTED':
                expected += 1
            else:
                diff += 1
            print(f'  ≠  {name}   [{tag}{": " + EXPECTED[name] if name in EXPECTED else ""}]')
            if na[0] != nb[0]:
                print(f'       status py={na[0]} rs={nb[0]}')
            if na[1] != nb[1]:
                print(f'       headers py={na[1]}\n               rs={nb[1]}')
            if na[2] != nb[2]:
                print(f'       body py={na[2][:400]!r}\n            rs={nb[2][:400]!r}')
        # HTTPS : certificat vérifié (PAS de -k), nom local.redlinks.fr.
        for side, port in (('py', PY_TLS), ('rs', RS_TLS)):
            try:
                line, body = https_status(port)
                print(f'  HTTPS {side}: {line} {body}')
            except Exception as e:
                print(f'  HTTPS {side}: ÉCHEC {type(e).__name__}: {e}')
                diff += 1
        print(f'\n{same} identiques, {expected} différences voulues, {diff} différences inattendues'
              f'{"" if iso_ok else " — montage ISO réel ignoré (udisksctl indisponible)"}')
        return 1 if diff else 0
    finally:
        for p in procs:
            try:
                os.killpg(p.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
        for p in procs:
            try:
                p.wait(5)
            except subprocess.TimeoutExpired:
                os.killpg(p.pid, signal.SIGKILL)
        if '--keep' not in sys.argv:
            shutil.rmtree(work, ignore_errors=True)
        else:
            print('gardé :', work)


if __name__ == '__main__':
    sys.exit(main())
