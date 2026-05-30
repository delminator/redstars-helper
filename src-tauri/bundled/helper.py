#!/usr/bin/env python3
"""Unified static + helper HTTP server for the autoencoder demo page.

Static files: served from this script's directory (the demo dir).
Helper API: under /helper/* — same-origin so it works from any client (mobile,
LAN, private window) without CORS dance.

Endpoints (all under /helper):
  GET  /helper/status         →  {"ok": true, "version": "..."}
  GET  /helper/lsusb          →  {"devices": [{bus, device, id, name}, ...]}
  GET  /helper/scale          →  latest scale reading (cached)
  POST /helper/enable-webgpu  →  appends WebGPU prefs to Firefox user.js
  POST /helper/reset-webgpu   →  removes WebGPU prefs
  POST /helper/redEC          →  body = binary file ; → {hash_hex, level} (auto Red1..Red4)
  POST /helper/redDEC         →  body = {"hash_hex": "<2048 chars>"} ; → {hashes_hex[1024], …}

Listens on 0.0.0.0:8080 by default so mobile devices on the same LAN can hit
the page (and the helper endpoints) via the desktop's IP.

Run: python3 helper.py
"""
import base64
import json
import os
import platform
import re
import shutil
import socket
import ssl
import subprocess
import threading
import time
import uuid
from http.server import BaseHTTPRequestHandler, HTTPServer, SimpleHTTPRequestHandler
from pathlib import Path
from urllib.parse import urlsplit, parse_qs

VERSION = '0.5.0'
PORT = int(os.environ.get('HELPER_PORT', '49080'))
HTTPS_PORT = int(os.environ.get('HELPER_HTTPS_PORT', '49443'))
DEMO_DIR = Path(__file__).resolve().parent
CERT_FILE = DEMO_DIR / 'cert.pem'
KEY_FILE = DEMO_DIR / 'key.pem'

# Origins allowed to call /helper/* across origins (HTTPS dashboard reaching
# https://local.redlinks.fr:8443/helper/* needs CORS to consent).
ALLOWED_ORIGINS = {
    'https://dev.redstars.redlinks.fr',
    'https://redstars.redlinks.fr',
    'http://localhost:49080',
    'https://local.redlinks.fr:49443',
    # Legacy — kept for older clients during the migration off 9999/8443.
    'http://localhost:9999',
    'https://local.redlinks.fr:8443',
}

# Codec autoencoder (redEC/redDEC) — chargement paresseux à la 1ʳᵉ requête /helper/redEC ou /redDEC.
# Permet au helper de démarrer même sans torch/numpy installés ; les routes renvoient une erreur
# claire si la dépendance manque.
_CODEC = {'loaded': False, 'redEC_chain': None, 'redDEC': None, 'err': None}

def _ensure_codec():
    if _CODEC['loaded'] or _CODEC['err']:
        return _CODEC['err']
    try:
        import sys as _sys
        if str(DEMO_DIR) not in _sys.path:
            _sys.path.insert(0, str(DEMO_DIR))
        from redEC import redEC_chain
        from redDEC import redDEC
        _CODEC['redEC_chain'] = redEC_chain
        _CODEC['redDEC'] = redDEC
        _CODEC['loaded'] = True
        return None
    except Exception as e:
        _CODEC['err'] = f'{type(e).__name__}: {e}'
        return _CODEC['err']

SCALE_PORT = '/dev/ttyUSB0'
SCALE_BAUD = 9600
# Format observed on cheap CH340-based scales: "WTST    +27.34  g"
SCALE_LINE = re.compile(r'(?P<status>[A-Z]{2,4})\s*(?P<sign>[+-])(?P<value>\d+(?:\.\d+)?)\s*(?P<unit>g|kg|lb|oz|ml)?', re.I)


class ScaleReader:
    """Background thread reading the scale's serial output and caching the
    latest stable value. /scale endpoint just returns the cache.

    Auto-reconnects if the serial port disappears (unplug/replug).
    """
    def __init__(self, port=SCALE_PORT, baud=SCALE_BAUD):
        self.port = port
        self.baud = baud
        self.lock = threading.Lock()
        self.state = {
            'connected': False, 'value': None, 'unit': None, 'sign': None,
            'status': None, 'raw': None, 'updated_at': None, 'error': None,
        }
        self._stop = threading.Event()
        threading.Thread(target=self._run, daemon=True).start()

    def _set(self, **kw):
        with self.lock:
            self.state.update(kw)

    def get(self):
        with self.lock:
            return dict(self.state)

    def _run(self):
        try:
            import serial
        except ImportError:
            self._set(error='pyserial not installed (pip install --user pyserial)')
            return
        while not self._stop.is_set():
            try:
                with serial.Serial(self.port, self.baud, timeout=1) as ser:
                    self._set(connected=True, error=None)
                    while not self._stop.is_set():
                        raw = ser.readline().decode('ascii', errors='replace').strip()
                        if not raw:
                            continue
                        m = SCALE_LINE.search(raw)
                        if m:
                            sign = -1 if m.group('sign') == '-' else 1
                            self._set(
                                connected=True,
                                value=sign * float(m.group('value')),
                                unit=(m.group('unit') or 'g').lower(),
                                sign=m.group('sign'),
                                status=m.group('status'),
                                raw=raw,
                                updated_at=time.time(),
                                error=None,
                            )
                        else:
                            # Boot messages, blank lines, etc — keep raw for debug
                            self._set(raw=raw, updated_at=time.time(), error=None)
            except FileNotFoundError:
                self._set(connected=False, error=f'{self.port} not present (scale unplugged?)')
                time.sleep(2)
            except PermissionError:
                self._set(connected=False, error=f'{self.port} permission denied (udev rule?)')
                time.sleep(5)
            except Exception as e:
                self._set(connected=False, error=type(e).__name__ + ': ' + str(e))
                time.sleep(2)


SCALE = ScaleReader()


def find_firefox_default_profile():
    """Return the path to the active Firefox profile, or None.

    Reads profiles.ini and prefers the one referenced by [Install*] Default=,
    which is what Firefox actually launches.
    """
    ff_dir = Path.home() / '.mozilla' / 'firefox'
    ini = ff_dir / 'profiles.ini'
    if not ini.exists():
        return None
    text = ini.read_text()
    # Find the Install section's Default first (this wins over Profile.Default=1)
    install_default = re.search(r'\[Install[^\]]+\]\s*\nDefault=(\S+)', text)
    if install_default:
        candidate = ff_dir / install_default.group(1)
        if candidate.is_dir():
            return candidate
    # Fallback: any Profile with Default=1
    for block in re.split(r'\n(?=\[)', text):
        if 'Default=1' in block:
            m = re.search(r'Path=(\S+)', block)
            if m:
                candidate = ff_dir / m.group(1)
                if candidate.is_dir():
                    return candidate
    return None


def parse_lsusb_line(line):
    m = re.match(r'Bus (\d+) Device (\d+): ID (\S+) (.*)', line)
    if not m:
        return None
    return {
        'bus': m.group(1),
        'device': m.group(2),
        'id': m.group(3),
        'name': m.group(4).strip(),
    }


# ─── ISO mount/unmount ────────────────────────────────────────────────
#
# The dashboard asks the helper to wrap a payload (or nothing) in an
# ISO 9660 image and mount it on the host using the OS's native loop
# mechanism — no sudo, no FUSE install required on stock desktops.
# Unmount cleans up and deletes the temp .iso.
#
# Mount mechanism per OS (all userspace):
#   Linux   → udisksctl loop-setup + udisksctl mount   (Polkit, session)
#   macOS   → hdiutil attach
#   Windows → powershell Mount-DiskImage

ISO_CACHE_DIR = Path.home() / '.cache' / 'redstars-helper' / 'iso'
MOUNTED = {}  # iso_id → {'iso_path','mount_path','dev','label','created_at'}


def make_iso(label='REDSTARS', payload=None):
    """
    Build an ISO 9660+UDF disc image.

    The `payload` argument is the data-loading hook for the disc:
      - None              → 100% empty FS (just a labeled, navigable disc)
      - bytes / bytearray → one file `payload.bin` at the root
      - dict[str, bytes]  → those named files at the root

    Returns the path to the written .iso. Caller owns its lifecycle.
    """
    ISO_CACHE_DIR.mkdir(parents=True, exist_ok=True)
    iso_id = uuid.uuid4().hex[:12]
    iso_path = ISO_CACHE_DIR / f'{iso_id}.iso'
    src_dir = ISO_CACHE_DIR / f'src-{iso_id}'
    src_dir.mkdir()
    try:
        if isinstance(payload, (bytes, bytearray)):
            (src_dir / 'payload.bin').write_bytes(bytes(payload))
        elif isinstance(payload, dict):
            for name, data in payload.items():
                safe = Path(str(name)).name  # strip path components
                if not safe:
                    continue
                (src_dir / safe).write_bytes(bytes(data))
        # else (None) → src_dir stays empty → empty filesystem

        tool = next((t for t in ('xorrisofs', 'genisoimage', 'mkisofs')
                     if shutil.which(t)), None)
        if not tool:
            raise RuntimeError('No ISO tool (install xorriso / genisoimage / mkisofs).')
        safe_label = (label or 'REDSTARS')[:32].strip() or 'REDSTARS'
        cmd = [tool, '-iso-level', '3', '-R', '-J', '-no-pad',
               '-V', safe_label, '-o', str(iso_path), str(src_dir)]
        r = subprocess.run(cmd, capture_output=True, text=True)
        if r.returncode != 0:
            raise RuntimeError(f'{tool} failed: {r.stderr.strip()[:500]}')
        return iso_path
    finally:
        shutil.rmtree(src_dir, ignore_errors=True)


def _run(cmd, check=True):
    return subprocess.run(cmd, capture_output=True, text=True, check=check)


def mount_iso(iso_path):
    """Mount as the user via the OS-native mechanism. Returns (mount_path, dev)."""
    sysname = platform.system()
    if sysname == 'Linux':
        out = _run(['udisksctl', 'loop-setup', '-f', str(iso_path),
                    '--no-user-interaction']).stdout.strip()
        # "Mapped file file.iso as /dev/loop0."
        dev = out.split()[-1].rstrip('.')
        m = _run(['udisksctl', 'mount', '-b', dev, '--no-user-interaction']).stdout.strip()
        # "Mounted /dev/loop0 at /run/media/user/LABEL"
        mount_path = m.split(' at ', 1)[-1].rstrip('.').strip()
        return mount_path, dev
    if sysname == 'Darwin':
        out = _run(['hdiutil', 'attach', '-nobrowse', str(iso_path)]).stdout.strip()
        # Last non-empty line: "/dev/disk2   Apple_HFS    /Volumes/REDSTARS"
        last = [l for l in out.splitlines() if l.strip()][-1]
        parts = re.split(r'\s{2,}|\t', last.strip())
        dev = parts[0].strip()
        mount_path = parts[-1].strip()
        return mount_path, dev
    if sysname == 'Windows':
        ps = (f"$img = Mount-DiskImage -ImagePath '{iso_path}' -PassThru; "
              "Start-Sleep -Milliseconds 250; "
              "($img | Get-Volume).DriveLetter")
        d = _run(['powershell', '-NoProfile', '-Command', ps]).stdout.strip().splitlines()[-1].strip()
        return f'{d}:\\', str(iso_path)  # dev = iso path (used by Dismount-DiskImage)
    raise RuntimeError(f'Unsupported OS: {sysname}')


def unmount_iso(iso_path, dev):
    sysname = platform.system()
    try:
        if sysname == 'Linux':
            _run(['udisksctl', 'unmount', '-b', dev, '--no-user-interaction'], check=False)
            _run(['udisksctl', 'loop-delete', '-b', dev, '--no-user-interaction'], check=False)
        elif sysname == 'Darwin':
            _run(['hdiutil', 'detach', dev], check=False)
        elif sysname == 'Windows':
            _run(['powershell', '-NoProfile', '-Command',
                  f"Dismount-DiskImage -ImagePath '{iso_path}'"], check=False)
    except Exception:
        pass


def list_mount(mount_path):
    """Lightweight directory listing for the dashboard frame."""
    out = []
    try:
        for name in sorted(os.listdir(mount_path)):
            full = os.path.join(mount_path, name)
            try:
                st = os.stat(full)
                out.append({
                    'name': name,
                    'path': full,
                    'size': st.st_size,
                    'is_dir': os.path.isdir(full),
                })
            except OSError:
                continue
    except FileNotFoundError:
        pass
    return out


class Handler(SimpleHTTPRequestHandler):
    """Same-origin server: static files + /helper/* API endpoints."""

    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=str(DEMO_DIR), **kwargs)

    def end_headers(self):
        # Allow CORS for /helper/* from the dev/prod dashboards. Same-origin
        # callers (page served by helper itself) get the headers too — no harm.
        origin = self.headers.get('Origin', '')
        if origin in ALLOWED_ORIGINS or self.path.startswith('/helper/'):
            allow = origin if origin in ALLOWED_ORIGINS else '*'
            self.send_header('Access-Control-Allow-Origin', allow)
            self.send_header('Access-Control-Allow-Methods', 'GET, POST, OPTIONS')
            self.send_header('Access-Control-Allow-Headers', 'Content-Type')
        # Cross-origin isolation for future SharedArrayBuffer / WASM threads.
        self.send_header('Cross-Origin-Opener-Policy', 'same-origin')
        self.send_header('Cross-Origin-Embedder-Policy', 'require-corp')
        self.send_header('Cross-Origin-Resource-Policy', 'cross-origin')
        super().end_headers()

    def do_OPTIONS(self):
        # Preflight for the dashboard's cross-origin /helper/* calls.
        self.send_response(204)
        self.end_headers()

    def _json(self, code, payload):
        body = json.dumps(payload).encode()
        self.send_response(code)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _read_json(self):
        n = int(self.headers.get('Content-Length', '0') or 0)
        if n <= 0:
            return {}
        try:
            return json.loads(self.rfile.read(n).decode('utf-8'))
        except Exception:
            return {}

    def do_GET(self):
        if not self.path.startswith('/helper/'):
            return super().do_GET()  # static file serve
        split = urlsplit(self.path)
        ep = split.path[len('/helper'):]  # strip prefix → /status, /lsusb, etc.
        query = parse_qs(split.query)
        if ep == '/status':
            self._json(200, {'ok': True, 'version': VERSION})
            return
        if ep == '/scale':
            state = SCALE.get()
            if state.get('updated_at'):
                state['age_ms'] = int((time.time() - state['updated_at']) * 1000)
            self._json(200, state)
            return
        if ep == '/lsusb':
            try:
                out = subprocess.run(['lsusb'], capture_output=True, text=True, timeout=2)
                if out.returncode != 0:
                    self._json(500, {'error': 'lsusb exit ' + str(out.returncode), 'stderr': out.stderr})
                    return
                devices = [d for d in (parse_lsusb_line(l) for l in out.stdout.splitlines()) if d]
                self._json(200, {'devices': devices, 'count': len(devices)})
            except FileNotFoundError:
                self._json(500, {'error': 'lsusb not installed'})
            except Exception as e:
                self._json(500, {'error': type(e).__name__ + ': ' + str(e)})
            return
        if ep == '/iso/list':
            iso_id = (query.get('id', ['']) or [''])[0]
            info = MOUNTED.get(iso_id)
            if not info:
                self._json(404, {'error': 'unknown id'})
                return
            self._json(200, {
                'mount_path': info['mount_path'],
                'label': info['label'],
                'entries': list_mount(info['mount_path']),
            })
            return
        self._json(404, {'error': 'unknown helper endpoint'})

    def do_HEAD(self):
        if not self.path.startswith('/helper/'):
            return super().do_HEAD()
        # Helper endpoints don't really do HEAD, just say OK.
        self.send_response(200)
        self.end_headers()

    def do_POST(self):
        if not self.path.startswith('/helper/'):
            self._json(404, {'error': 'POST only allowed under /helper/*'})
            return
        ep = self.path[len('/helper'):]
        if ep == '/enable-webgpu':
            profile = find_firefox_default_profile()
            if profile is None:
                self._json(500, {'error': 'No Firefox profile found at ~/.mozilla/firefox'})
                return
            user_js = profile / 'user.js'
            content = user_js.read_text() if user_js.exists() else ''
            additions = []
            for pref in ('dom.webgpu.enabled', 'dom.webgpu.unsafe.enabled'):
                if pref not in content:
                    additions.append(f'user_pref("{pref}", true);')
            if additions:
                header = '' if content.endswith('\n') or not content else '\n'
                user_js.write_text(content + header + '\n'.join(additions) + '\n')
            self._json(200, {
                'ok': True,
                'profile': str(profile),
                'user_js': str(user_js),
                'wrote': len(additions),
                'restart_required': True,
                'message': 'Restart Firefox for changes to take effect.' if additions
                           else 'WebGPU prefs already present in user.js.',
            })
            return

        if ep == '/reset-webgpu':
            # Strip our prefs from user.js AND force them back to default in
            # prefs.js so the next Firefox launch doesn't see cached `true`.
            profile = find_firefox_default_profile()
            if profile is None:
                self._json(500, {'error': 'No Firefox profile found at ~/.mozilla/firefox'})
                return
            actions = []
            user_js = profile / 'user.js'
            if user_js.exists():
                kept = []
                for line in user_js.read_text().splitlines():
                    if 'dom.webgpu.enabled' in line or 'dom.webgpu.unsafe.enabled' in line:
                        continue
                    if line.startswith('//') and 'WebGPU' in line:
                        continue
                    kept.append(line)
                new_content = '\n'.join(kept).strip()
                if new_content:
                    user_js.write_text(new_content + '\n')
                else:
                    user_js.unlink()
                actions.append('cleaned user.js')
            # prefs.js is rewritten by Firefox at shutdown — best-effort patch.
            # We rewrite the lines while Firefox is closed; if it's running we
            # warn the caller.
            prefs_js = profile / 'prefs.js'
            if prefs_js.exists():
                lines = prefs_js.read_text().splitlines()
                kept = [l for l in lines if 'dom.webgpu.enabled' not in l and 'dom.webgpu.unsafe.enabled' not in l]
                if len(kept) != len(lines):
                    prefs_js.write_text('\n'.join(kept) + '\n')
                    actions.append('patched prefs.js (will only stick if Firefox is closed)')
            self._json(200, {
                'ok': True,
                'profile': str(profile),
                'user_js': str(user_js),
                'action': ', '.join(actions) or 'nothing to do',
                'message': 'Close Firefox first, then reopen, for the reset to apply cleanly.',
            })
            return

        if ep == '/iso/mount':
            body = self._read_json()
            label = (body.get('label') or 'REDSTARS')
            payload = None
            # Two body shapes accepted:
            #   {"files": {"name.ext": "<base64>", ...}}   → multi-file ISO
            #   {"payload": "<base64>"}                    → single payload.bin
            # `files` wins if both are present.
            if isinstance(body.get('files'), dict) and body['files']:
                try:
                    payload = {name: base64.b64decode(b64)
                               for name, b64 in body['files'].items()}
                except Exception as e:
                    self._json(400, {'error': f'bad base64 in files: {e}'})
                    return
            elif body.get('payload'):
                try:
                    payload = base64.b64decode(body['payload'])
                except Exception as e:
                    self._json(400, {'error': f'bad base64 payload: {e}'})
                    return
            try:
                iso_path = make_iso(label, payload)
                mount_path, dev = mount_iso(iso_path)
            except Exception as e:
                self._json(500, {'error': type(e).__name__ + ': ' + str(e)})
                return
            iso_id = uuid.uuid4().hex[:12]
            MOUNTED[iso_id] = {
                'iso_path': str(iso_path),
                'mount_path': mount_path,
                'dev': dev,
                'label': label,
                'created_at': time.time(),
            }
            self._json(200, {
                'id': iso_id,
                'mount_path': mount_path,
                'label': label,
                'entries': list_mount(mount_path),
            })
            return

        if ep == '/iso/unmount':
            body = self._read_json()
            iso_id = body.get('id')
            info = MOUNTED.pop(iso_id, None)
            if not info:
                self._json(404, {'error': 'unknown id'})
                return
            unmount_iso(info['iso_path'], info['dev'])
            try:
                os.remove(info['iso_path'])
            except OSError:
                pass
            self._json(200, {'ok': True, 'id': iso_id})
            return

        if ep == '/iso/open':
            body = self._read_json()
            path = body.get('path', '')
            # Path must sit under a currently-mounted iso (we never open
            # arbitrary host paths on behalf of the dashboard).
            if not any(path.startswith(info['mount_path']) for info in MOUNTED.values()):
                self._json(403, {'error': 'path not in a mounted iso'})
                return
            sysname = platform.system()
            try:
                if sysname == 'Linux':
                    subprocess.Popen(['xdg-open', path])
                elif sysname == 'Darwin':
                    subprocess.Popen(['open', path])
                elif sysname == 'Windows':
                    os.startfile(path)  # type: ignore[attr-defined]
                else:
                    self._json(500, {'error': f'open unsupported on {sysname}'})
                    return
                self._json(200, {'ok': True})
            except Exception as e:
                self._json(500, {'error': type(e).__name__ + ': ' + str(e)})
            return

        if ep == '/redEC':
            err = _ensure_codec()
            if err:
                self._json(500, {'error': f'codec load failed: {err}', 'hint': 'pip install torch numpy'}); return
            n = int(self.headers.get('Content-Length', '0') or 0)
            if n <= 0:
                self._json(400, {'error': 'empty body — POST raw binary as application/octet-stream'}); return
            raw = self.rfile.read(n)
            import tempfile
            with tempfile.TemporaryDirectory() as td:
                in_path  = Path(td) / 'in.bin'
                out_path = Path(td) / 'out.bin'
                in_path.write_bytes(raw)
                try:
                    level, h, in_size = _CODEC['redEC_chain'](in_path, out_path)
                    self._json(200, {
                        'ok': True,
                        'level': f'Red{level}',
                        'n_chain_steps': level,
                        'input_bytes': in_size,
                        'output_hash_hex': h.hex(),
                        'output_bytes': len(h),
                    })
                except Exception as e:
                    self._json(500, {'error': f'{type(e).__name__}: {e}'})
            return

        if ep == '/redDEC':
            err = _ensure_codec()
            if err:
                self._json(500, {'error': f'codec load failed: {err}', 'hint': 'pip install torch numpy'}); return
            body = self._read_json()
            hash_hex = (body.get('hash_hex') or '').strip().lower()
            if len(hash_hex) != 2048 or any(c not in '0123456789abcdef' for c in hash_hex):
                self._json(400, {'error': 'hash_hex must be exactly 2048 hex chars (= 8192 bits = 1 BYTEA)'}); return
            try:
                out_bytes  = _CODEC['redDEC'](bytes.fromhex(hash_hex))
                n_hashes   = len(out_bytes) // 1024
                hashes_hex = [out_bytes[i*1024:(i+1)*1024].hex() for i in range(n_hashes)]
                n_distinct = len(set(hashes_hex))
                self._json(200, {
                    'ok': True,
                    'input_hash_hex': hash_hex,
                    'output_bytes': len(out_bytes),
                    'n_hashes': n_hashes,
                    'n_distinct': n_distinct,
                    'hashes_hex': hashes_hex,
                })
            except Exception as e:
                self._json(500, {'error': f'{type(e).__name__}: {e}'})
            return

        self._json(404, {'error': 'unknown helper endpoint'})

    def log_message(self, fmt, *args):
        # Silent — uncomment to debug
        pass


def _serve_thread(server, label):
    print(f'  {label}: ready')
    try:
        server.serve_forever()
    except Exception as e:
        print(f'  {label} crashed: {e}')


def main():
    # HTTP server on :8080 — page + helper API, same-origin path.
    http_srv = HTTPServer(('0.0.0.0', PORT), Handler)
    print(f'redstars-helper {VERSION}')
    print(f'  static files from {DEMO_DIR}')

    print(f'  HTTP  http://0.0.0.0:{PORT}/  +  /helper/*')

    # HTTPS server on :8443 with the local.redlinks.fr cert — required for
    # HTTPS dashboards (dev.redstars.redlinks.fr) to reach /helper/* without
    # mixed-content blocks. local.redlinks.fr is a public DNS A record
    # pointing at 127.0.0.1; the cert is from Let's Encrypt DNS-01.
    https_thread = None
    if CERT_FILE.exists() and KEY_FILE.exists():
        ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        ctx.load_cert_chain(certfile=str(CERT_FILE), keyfile=str(KEY_FILE))
        # Bind 127.0.0.1 only — the cert is for local.redlinks.fr which always
        # resolves to 127.0.0.1, and we don't want to expose the helper to LAN
        # over HTTPS (the cert/key would let any LAN box impersonate us).
        https_srv = HTTPServer(('127.0.0.1', HTTPS_PORT), Handler)
        https_srv.socket = ctx.wrap_socket(https_srv.socket, server_side=True)
        print(f'  HTTPS https://local.redlinks.fr:{HTTPS_PORT}/  +  /helper/*')
        https_thread = threading.Thread(target=_serve_thread, args=(https_srv, 'HTTPS'), daemon=True)
        https_thread.start()
    else:
        print(f'  HTTPS: skipped — missing {CERT_FILE.name} / {KEY_FILE.name}')
    try:
        http_srv.serve_forever()
    except KeyboardInterrupt:
        print('\nbye')


if __name__ == '__main__':
    main()
