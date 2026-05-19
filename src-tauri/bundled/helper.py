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

Listens on 0.0.0.0:8080 by default so mobile devices on the same LAN can hit
the page (and the helper endpoints) via the desktop's IP.

Run: python3 helper.py
"""
import json
import os
import re
import socket
import ssl
import subprocess
import threading
import time
from http.server import BaseHTTPRequestHandler, HTTPServer, SimpleHTTPRequestHandler
from pathlib import Path

VERSION = '0.4.0'
PORT = int(os.environ.get('HELPER_PORT', '9999'))
HTTPS_PORT = int(os.environ.get('HELPER_HTTPS_PORT', '8443'))
DEMO_DIR = Path(__file__).resolve().parent
CERT_FILE = DEMO_DIR / 'cert.pem'
KEY_FILE = DEMO_DIR / 'key.pem'

# Origins allowed to call /helper/* across origins (HTTPS dashboard reaching
# https://local.redlinks.fr:8443/helper/* needs CORS to consent).
ALLOWED_ORIGINS = {
    'https://dev.redstars.redlinks.fr',
    'https://redstars.redlinks.fr',
    'http://localhost:9999',
    'https://local.redlinks.fr:8443',
}

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

    def do_GET(self):
        if not self.path.startswith('/helper/'):
            return super().do_GET()  # static file serve
        ep = self.path[len('/helper'):]  # strip prefix → /status, /lsusb, etc.
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
