#!/usr/bin/env python3
"""RedStars helper — the local HTTP(S) API used by the site, the console client and
the tray shell. Everything lives under /helper/*; any other path answers 404.

0.5.64 removed the codec (redEC / redDEC, the étalement chain, its benches, jobs
and blob store), the embedded demo page and its float image models — along with
the static file server that exposed them. Not deleted: archived, whole, at tag
`archive/codec-demo-20260914` of the redstars-helper repo.

The static server also served everything next to this script, cert.pem and
key.pem included, on 0.0.0.0. It is gone with the page.

Endpoints (all under /helper):
  GET  /helper/status         →  {"ok": true, "version": "..."}
  GET  /helper/ping           →  {"ok": true} — battement du site ouvert (voir IDLE_EXIT_S)
  GET  /helper/lsusb          →  {"devices": [{bus, device, id, name}, ...]}
  GET  /helper/scale          →  scale reading (on-demand; the serial port is
                                  opened only while the page keeps polling)
  POST /helper/enable-webgpu  →  appends WebGPU prefs to Firefox user.js
  POST /helper/reset-webgpu   →  removes WebGPU prefs
  GET  /helper/files/pick     →  picker natif multi-fichiers ; → {paths, entries}
  POST /helper/refs/mount     →  {paths,label?} ; symlinks dans un tmpdir, no copy → {id,mount_path,entries}
  POST /helper/refs/open      →  {id,path?} ; xdg-open le tmpdir ou un de ses fichiers
  POST /helper/refs/unmount   →  {id} ; supprime les symlinks (fichiers cibles intacts)
  GET  /helper/refs/list?id=  →  {entries} du tmpdir

Listens on 0.0.0.0:49080 (HTTP) and 0.0.0.0:49443 (HTTPS) by default so mobile
devices on the same LAN can hit the helper endpoints via the
desktop's IP. Ports are IANA dynamic range — no collision with standard apps.

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
import tarfile
import threading
import time
import uuid
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path
from urllib.parse import urlsplit, parse_qs

VERSION = '0.5.64'
PORT = int(os.environ.get('HELPER_PORT', '49080'))
HTTPS_PORT = int(os.environ.get('HELPER_HTTPS_PORT', '49443'))
DEMO_DIR = Path(__file__).resolve().parent
CERT_FILE = DEMO_DIR / 'cert.pem'
KEY_FILE = DEMO_DIR / 'key.pem'

# Embedded local.redlinks.fr cert + key (Let's Encrypt). Used as a last
# resort when the file-on-disk versions aren't reachable — typical after
# auto-update of helper.py to the OS cache dir, which leaves the bundled
# certs unreachable. Renew window: see `openssl x509 -enddate -in cert.pem`.
_EMBEDDED_CERT_B64 = 'LS0tLS1CRUdJTiBDRVJUSUZJQ0FURS0tLS0tCk1JSURrRENDQXhhZ0F3SUJBZ0lTQllRZHBxRmdEaURrUXMvK1licXhpQWkyTUFvR0NDcUdTTTQ5QkFNRE1ETXgKQ3pBSkJnTlZCQVlUQWxWVE1SWXdGQVlEVlFRS0V3MU1aWFFuY3lCRmJtTnllWEIwTVF3d0NnWURWUVFERXdOWgpSVEl3SGhjTk1qWXdPREkyTVRJME1qSTFXaGNOTWpZeE1USTBNVEkwTWpJMFdqQWNNUm93R0FZRFZRUURFeEZzCmIyTmhiQzV5WldSc2FXNXJjeTVtY2pCWk1CTUdCeXFHU000OUFnRUdDQ3FHU000OUF3RUhBMElBQkNiSGJqQXkKZXkvWS8xa3pxZlU3enBSUDBjVk1QejZLeUxxSGNscEpOVUtsanJaRExKeXhRWVBVSVpvQ0dOSXpoOUdaeEVvQQpISzcxazVJTEhXZzR1QitqZ2dJZk1JSUNHekFPQmdOVkhROEJBZjhFQkFNQ0I0QXdFd1lEVlIwbEJBd3dDZ1lJCkt3WUJCUVVIQXdFd0RBWURWUjBUQVFIL0JBSXdBREFkQmdOVkhRNEVGZ1FVVFlyc240WjhmYUZGcFdNODMra0cKOUt6eHJONHdId1lEVlIwakJCZ3dGb0FVdVZueWpzOGk4SWJUTjBqL2RoUVl1b0xZVlljd013WUlLd1lCQlFVSApBUUVFSnpBbE1DTUdDQ3NHQVFVRkJ6QUNoaGRvZEhSd09pOHZlV1V5TG1rdWJHVnVZM0l1YjNKbkx6QWNCZ05WCkhSRUVGVEFUZ2hGc2IyTmhiQzV5WldSc2FXNXJjeTVtY2pBVEJnTlZIU0FFRERBS01BZ0dCbWVCREFFQ0FUQXUKQmdOVkhSOEVKekFsTUNPZ0lhQWZoaDFvZEhSd09pOHZlV1V5TG1NdWJHVnVZM0l1YjNKbkx6VTFMbU55YkRDQwpBUXdHQ2lzR0FRUUIxbmtDQkFJRWdmMEVnZm9BK0FCMUFNczQ5eFdKZklTaFJGOWJ3ZDM3eVc3eW1sbk5Sd3BwCkJZV3d5eFRERkZqbkFBQUJvRDVOWitrQUFBUURBRVl3UkFJZ0hZblZCZWMwWm5GbXJpOENlMmlxdzBNdFdSamEKS2h6eXF0VThlelpEclhvQ0lBeFFJaktLSUNCd1FoMGc1c3Z5Q3VwVUpNTHBUclVjMCtWTVVQRGp4akp1QUg4QQpKdU5rYmxocElTTzhORDlISkRXYk41TE5KRnFJMkJYVGt6UDltUmlyUnlNQUFBR2dQazFodXdBSUFBQUZBRGxlCjVRd0VBd0JJTUVZQ0lRREJXVGJ5ZjAwQm5LWStsSUVRa3k0VHUxQjZzaGdaQjZwazNPNFA5VDlpRlFJaEFOUEoKTjhhK0dpT0pneWh2aDgrVk1HQkdwdHJKOXV4VEt6bHlEeXdRWDhMME1Bb0dDQ3FHU000OUJBTURBMmdBTUdVQwpNUURCTFFEN2R3dXQzK2lJaDRQTnViTkovNnBmMUpqL3UxS2VFblVHa2Q0L0ZrQ082alpLZjBFc2ZnMnhCdnNiCjA4c0NNQjd2R28xZW1NS0xWcVFDU3FORXN1QjJWdHpkOGxMSURQTEFYWDlxZnpMTTErSUpTMXFKTVBOVXQ3YUcKSmZkekdnPT0KLS0tLS1FTkQgQ0VSVElGSUNBVEUtLS0tLQotLS0tLUJFR0lOIENFUlRJRklDQVRFLS0tLS0KTUlJQ2pEQ0NBaEdnQXdJQkFnSVFUZk94WGRiQWVFeFFmTk43V09ieEZUQUtCZ2dxaGtqT1BRUURBekF1TVFzdwpDUVlEVlFRR0V3SlZVekVOTUFzR0ExVUVDaE1FU1ZOU1J6RVFNQTRHQTFVRUF4TUhVbTl2ZENCWlJUQWVGdzB5Ck5UQTVNRE13TURBd01EQmFGdzB5T0RBNU1ESXlNelU1TlRsYU1ETXhDekFKQmdOVkJBWVRBbFZUTVJZd0ZBWUQKVlFRS0V3MU1aWFFuY3lCRmJtTnllWEIwTVF3d0NnWURWUVFERXdOWlJUSXdkakFRQmdjcWhrak9QUUlCQmdVcgpnUVFBSWdOaUFBUnhtclF6a2RiRUVMM01xWHQzZEpRdHRZYzQ3YXhrZERUSHVkNVRQcU0yejV1U0Q1Y21rMFdyCkhsV1h2bmx2cUJMcWlCMzRrbHV4SWJtTXlBaXEzL1lENmU4MC92VjI1OUs4WFFJZGpGWGxvWU9hMG1JVTcxZjcKSFEwOVB2WURsdytqZ2U0d2dlc3dEZ1lEVlIwUEFRSC9CQVFEQWdHR01CTUdBMVVkSlFRTU1Bb0dDQ3NHQVFVRgpCd01CTUJJR0ExVWRFd0VCL3dRSU1BWUJBZjhDQVFBd0hRWURWUjBPQkJZRUZMbFo4bzdQSXZDRzB6ZEkvM1lVCkdMcUMyRldITUI4R0ExVWRJd1FZTUJhQUZLUElKbHFPb1V6UU5XUDhteVBJT3E1VzgwOVdNRElHQ0NzR0FRVUYKQndFQkJDWXdKREFpQmdnckJnRUZCUWN3QW9ZV2FIUjBjRG92TDNsbExta3ViR1Z1WTNJdWIzSm5MekFUQmdOVgpIU0FFRERBS01BZ0dCbWVCREFFQ0FUQW5CZ05WSFI4RUlEQWVNQnlnR3FBWWhoWm9kSFJ3T2k4dmVXVXVZeTVzClpXNWpjaTV2Y21jdk1Bb0dDQ3FHU000OUJBTURBMmtBTUdZQ01RREljbnc1ZGNaTE45ZmZ5blhubmtMRC9pdFMKSkV5Y0pQYjNzUmt6ZXFCb3d1cDd2T3NBd2Fxb0NuTm4vamg5d3ljQ01RQ0pNNkNQbGFPQzRwUVlZYkp0VlBZYgpES3JJYjJFS2s1TnBPcEU2L1h0dFFZWlYvM2dpbEI5bCtDYy9ET1Z3bXlnPQotLS0tLUVORCBDRVJUSUZJQ0FURS0tLS0tCi0tLS0tQkVHSU4gQ0VSVElGSUNBVEUtLS0tLQpNSUlDcGpDQ0FpdWdBd0lCQWdJUkFJY2haZncwdHVYN3FLM1ZzM0JmdFRvd0NnWUlLb1pJemowRUF3TXdUekVMCk1Ba0dBMVVFQmhNQ1ZWTXhLVEFuQmdOVkJBb1RJRWx1ZEdWeWJtVjBJRk5sWTNWeWFYUjVJRkpsYzJWaGNtTm8KSUVkeWIzVndNUlV3RXdZRFZRUURFd3hKVTFKSElGSnZiM1FnV0RJd0hoY05Nall3TlRFek1EQXdNREF3V2hjTgpNekl3T1RBeU1qTTFPVFU1V2pBdU1Rc3dDUVlEVlFRR0V3SlZVekVOTUFzR0ExVUVDaE1FU1ZOU1J6RVFNQTRHCkExVUVBeE1IVW05dmRDQlpSVEIyTUJBR0J5cUdTTTQ5QWdFR0JTdUJCQUFpQTJJQUJEd1MvNnZocmNWcWNiQm8KK3dnZEkzZnduOXg3RE5KSk9ZL2xUT3RpMHZrd3VSTjg3UmhFaFRIMTdFN1h5RmpXc1BZaElQdC93ek9xeFRkMgpiKzRaSk55OUlEMDRZeXdGOVU1emFzRFZ5R1NORXJWTnR6OHVTR2g1aXpXODdqNzdHYU9CNnpDQjZEQU9CZ05WCkhROEJBZjhFQkFNQ0FRWXdFd1lEVlIwbEJBd3dDZ1lJS3dZQkJRVUhBd0V3RHdZRFZSMFRBUUgvQkFVd0F3RUIKL3pBZEJnTlZIUTRFRmdRVW84Z21XbzZoVE5BMVkveWJJOGc2cmxielQxWXdId1lEVlIwakJCZ3dGb0FVZkVLVwpydDVMU0R2Nmt2aWVqTTl0aTZseU41VXdNZ1lJS3dZQkJRVUhBUUVFSmpBa01DSUdDQ3NHQVFVRkJ6QUNoaFpvCmRIUndPaTh2ZURJdWFTNXNaVzVqY2k1dmNtY3ZNQk1HQTFVZElBUU1NQW93Q0FZR1o0RU1BUUlCTUNjR0ExVWQKSHdRZ01CNHdIS0Fhb0JpR0ZtaDBkSEE2THk5NE1pNWpMbXhsYm1OeUxtOXlaeTh3Q2dZSUtvWkl6ajBFQXdNRAphUUF3WmdJeEFNVTE5V0N0bXhWTkQ4VUhCWlJvbWE0OVo3alBzNjREbWEwZVR1MU9DaFZiQi8ySjdHVjNudllLCkF4NTR1azFHOVFJeEFPMG1pTFZKdThQTE5pWFhYa2lFL2dzSzNDVFJURi9hZW80Yk1YNDJadzQwY3NSVTZBQzIKNmhTVzEvSVdhYXM2ZGc9PQotLS0tLUVORCBDRVJUSUZJQ0FURS0tLS0tCi0tLS0tQkVHSU4gQ0VSVElGSUNBVEUtLS0tLQpNSUlFY0RDQ0FsaWdBd0lCQWdJUWJJOGR4eWZIRVg5N3I0VTZ5WUQ1elRBTkJna3Foa2lHOXcwQkFRc0ZBREJQCk1Rc3dDUVlEVlFRR0V3SlZVekVwTUNjR0ExVUVDaE1nU1c1MFpYSnVaWFFnVTJWamRYSnBkSGtnVW1WelpXRnkKWTJnZ1IzSnZkWEF4RlRBVEJnTlZCQU1UREVsVFVrY2dVbTl2ZENCWU1UQWVGdzB5TmpBMU1UTXdNREF3TURCYQpGdzB6TWpBNU1ESXlNelU1TlRsYU1FOHhDekFKQmdOVkJBWVRBbFZUTVNrd0p3WURWUVFLRXlCSmJuUmxjbTVsCmRDQlRaV04xY21sMGVTQlNaWE5sWVhKamFDQkhjbTkxY0RFVk1CTUdBMVVFQXhNTVNWTlNSeUJTYjI5MElGZ3kKTUhZd0VBWUhLb1pJemowQ0FRWUZLNEVFQUNJRFlnQUV6WnZWbjRDREN1d0pTdk1XU2o1Y3ozZXMzbWNGRFIwSAp0dHdXKzFxTEZOdmljV0RFdWtXVkVZbU82Z2JmOXlvV0hLUzV4Y1V5NEFQZ0hvSVlPSXZYUmRnS2FtN21BSGY3CkFsRjlJdGdLYnBwYmQ5L3cra0hzT2R4MXltZ0hEQi9xbzRIMU1JSHlNQTRHQTFVZER3RUIvd1FFQXdJQkJqQWQKQmdOVkhTVUVGakFVQmdnckJnRUZCUWNEQVFZSUt3WUJCUVVIQXdJd0R3WURWUjBUQVFIL0JBVXdBd0VCL3pBZApCZ05WSFE0RUZnUVVmRUtXcnQ1TFNEdjZrdmllak05dGk2bHlONVV3SHdZRFZSMGpCQmd3Rm9BVWViUlo1bnUyCjVlUUJjNEFJaU1nYVdQYnBtMjR3TWdZSUt3WUJCUVVIQVFFRUpqQWtNQ0lHQ0NzR0FRVUZCekFDaGhab2RIUncKT2k4dmVERXVhUzVzWlc1amNpNXZjbWN2TUJNR0ExVWRJQVFNTUFvd0NBWUdaNEVNQVFJQk1DY0dBMVVkSHdRZwpNQjR3SEtBYW9CaUdGbWgwZEhBNkx5OTRNUzVqTG14bGJtTnlMbTl5Wnk4d0RRWUpLb1pJaHZjTkFRRUxCUUFECmdnSUJBRDIvZTlmcm1NeE5wQ1YwM3FVSGVnZytNVjJ3ejk2NDRZb1hkcXRIOFJ5V1ljQk83eGZqakdFWGRVMWUKL28wT2tFRml5blVDT1NJay92TExvN3R0ejZDUEFlTmxXZkMwWE5rb0dlV2dLNmpqWHZvekJhR3VHSDVuMFVmbwpzaE1lV1R1VVJxTk41RzAwc1NYRFRCcnBwMittZ3ZkWlFqYjhLMTFUWU1BMjVRQStZSE5mYklFTDBCbmlBaEtTCjJnc25KalN6cmRaTEkrRVo3U0V5cWRSMnJramQxS3V0TERVK24zVEZ5eGpuaVpWR3VyNFlsaE1QM21ZL2RWOTUKSXJ1QWtrak9aaWVyNmhHQmRFZ1pYWHZhQ3o5dTlpVkVhZHNJRTc1cEFHTDhvSFY1dnhkQVJEaW90UnB1bDFJTgovVVp3ekFicmZVRmN3MUhrQWNZRC9tbFpmblEyaWVDRjJNUzdqM1ZodjdKUERLcDQ1Zm15a216WU5TcnVtUlcwCnVwRkZLREJPb0Y3aHNPYjdvTHlIUytVZnQ2ak9VZk9yb2dqOFlVeDM4aEtiMksyMHI0Mk9nc1NkRGR4ZGVZV2MKTVMzU2I2bXdKZVNaRVl4SjJnYVhuRFNQYUtoaHJOa1l3bGp5VlF5cjROcStNRUp5dFhOVG5IcWFBY3JOd1psVgpwY0pMMUtCbk1yTWpQN2VhbnZVd0wzRllqM2NGMTdqdGJvTHQ3Z0xvaTQrMnJXWkZ2bit3NTRqbWQvRkl1aGhaCmNFYVUvd3ZVNkJVTk10Y1ZxdVZHSHA3aXRRZUR0aDVqK1hMM2o0V0oyU0FCd3pVbDZPZVlkZ3BJdC9JVFphK3AKVFQwbVEvcjVYeUE0TUVBaWFibjdYSmp2Q0VSbEYyZGNuMndxSncrQ3JlVGtrUTJSCi0tLS0tRU5EIENFUlRJRklDQVRFLS0tLS0K'
_EMBEDDED_KEY_B64 = 'LS0tLS1CRUdJTiBQUklWQVRFIEtFWS0tLS0tCk1JR0hBZ0VBTUJNR0J5cUdTTTQ5QWdFR0NDcUdTTTQ5QXdFSEJHMHdhd0lCQVFRZ0srUUREaGt1c2VVNXNqeGYKL3JqanYrTE5QTWJpUVpIVmVlOXRveGkrMFltaFJBTkNBQVFteDI0d01uc3YyUDlaTTZuMU84NlVUOUhGVEQ4Kwppc2k2aDNKYVNUVkNwWTYyUXl5Y3NVR0QxQ0dhQWhqU000ZlJtY1JLQUJ5dTlaT1NDeDFvT0xnZgotLS0tLUVORCBQUklWQVRFIEtFWS0tLS0tCg=='


def _resolve_cert_paths():
    """Find usable cert.pem + key.pem on disk; fall back to materializing
    the embedded copy in a tmp dir. Resolution order:
      1. DEMO_DIR (where helper.py is currently running from)
      2. $REDSTARS_HELPER_BUNDLED_DIR (passed by the Tauri shell if it
         knows the bundle path; absent on auto-update runs)
      3. embedded fallback → /tmp/redstars-helper-certs/
    Returns (cert_path, key_path) or (None, None) if even the embed
    write fails."""
    import tempfile as _tf
    candidates = [DEMO_DIR]
    bundled = os.environ.get('REDSTARS_HELPER_BUNDLED_DIR')
    if bundled:
        candidates.append(Path(bundled))
    for d in candidates:
        c = d / 'cert.pem'
        k = d / 'key.pem'
        if c.is_file() and k.is_file():
            return c, k
    try:
        tmp = Path(_tf.gettempdir()) / 'redstars-helper-certs'
        tmp.mkdir(exist_ok=True)
        c = tmp / 'cert.pem'
        k = tmp / 'key.pem'
        c.write_bytes(base64.b64decode(_EMBEDDED_CERT_B64))
        k.write_bytes(base64.b64decode(_EMBEDDED_KEY_B64))
        try: os.chmod(k, 0o600)
        except OSError: pass
        return c, k
    except Exception as e:
        print(f'  HTTPS: failed to materialize embedded certs: {e}')
        return None, None

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


SCALE_PORT = '/dev/ttyUSB0'
SCALE_BAUD = 9600
# Format observed on cheap CH340-based scales: "WTST    +27.34  g"
SCALE_LINE = re.compile(r'(?P<status>[A-Z]{2,4})\s*(?P<sign>[+-])(?P<value>\d+(?:\.\d+)?)\s*(?P<unit>g|kg|lb|oz|ml)?', re.I)


class _StdlibSerial:
    """Minimal stdlib (termios) serial reader — a pyserial-free fallback so the
    helper reads the scale without any pip install (the GUI-launched helper often
    picks a Python without pyserial). Linux only. Implements just the subset
    ScaleReader uses: in_waiting, readline(), close()."""
    def __init__(self, port, baud, timeout=0.4):
        import termios
        self.timeout = timeout
        self._buf = b''
        self.fd = os.open(port, os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
        try:
            attrs = termios.tcgetattr(self.fd)  # [iflag,oflag,cflag,lflag,ispeed,ospeed,cc]
            baud_const = getattr(termios, 'B%d' % baud, termios.B9600)
            attrs[0] = termios.IGNPAR
            attrs[1] = 0
            attrs[2] = (attrs[2] & ~termios.CSIZE) | termios.CS8 | termios.CLOCAL | termios.CREAD
            attrs[2] &= ~(termios.PARENB | termios.CSTOPB)
            if hasattr(termios, 'CRTSCTS'):
                attrs[2] &= ~termios.CRTSCTS
            attrs[3] = 0
            attrs[4] = baud_const
            attrs[5] = baud_const
            termios.tcsetattr(self.fd, termios.TCSANOW, attrs)
        except Exception:
            pass  # a pty / odd device may reject some attrs — raw read still works

    @property
    def in_waiting(self):
        import array
        import fcntl
        import termios
        buf = array.array('i', [0])
        try:
            fcntl.ioctl(self.fd, termios.FIONREAD, buf, True)
            return buf[0]
        except Exception:
            return 0

    def _pump(self):
        try:
            data = os.read(self.fd, 4096)
            if data:
                self._buf += data
        except (BlockingIOError, OSError):
            pass

    def readline(self):
        deadline = time.time() + self.timeout
        while b'\n' not in self._buf and b'\r' not in self._buf and time.time() < deadline:
            self._pump()
            if b'\n' not in self._buf and b'\r' not in self._buf:
                time.sleep(0.02)
        idx = -1
        for i, c in enumerate(self._buf):
            if c in (10, 13):
                idx = i
                break
        if idx < 0:
            line, self._buf = self._buf, b''
        else:
            line, self._buf = self._buf[:idx], self._buf[idx + 1:]
        return line

    def close(self):
        try:
            os.close(self.fd)
        except Exception:
            pass


def _open_scale_serial(port, baud, timeout=0.4):
    """Open the scale serial port. Prefer pyserial; fall back to the stdlib
    (termios) reader so the helper works even without a pyserial install."""
    try:
        import serial
        return serial.Serial(port, baud, timeout=timeout)
    except ImportError:
        return _StdlibSerial(port, baud, timeout=timeout)


class ScaleReader:
    """On-demand serial scale reader. There is NO 24/7 background polling: the
    serial port is opened lazily on the first /scale request and kept open only
    while the page keeps requesting. A single self-rescheduling idle timer (one
    wake every IDLE_CLOSE_SECS, ONLY while in use) closes the port a few seconds
    after the last read, so the helper goes fully dormant — zero threads, zero
    wakeups — when the scale isn't being used. The web page drives the cadence
    (poll ~1/s while the weighing screen is open).

    Cheap CH340 scales stream weight lines continuously while powered, so each
    read drains the serial buffer and returns the freshest line.
    """
    IDLE_CLOSE_SECS = 5

    def __init__(self, port=SCALE_PORT, baud=SCALE_BAUD):
        self.port = port
        self.baud = baud
        self.lock = threading.Lock()
        self._ser = None
        self._timer = None
        self._last_request = 0.0
        self._last = {
            'connected': False, 'value': None, 'unit': None, 'sign': None,
            'status': None, 'raw': None, 'updated_at': None, 'error': None,
        }

    # --- port lifecycle (caller holds self.lock) -------------------------
    def _close(self):
        if self._timer is not None:
            self._timer.cancel()
            self._timer = None
        if self._ser is not None:
            try:
                self._ser.close()
            except Exception:
                pass
            self._ser = None

    def _arm_timer(self):
        t = threading.Timer(self.IDLE_CLOSE_SECS, self._on_timer)
        t.daemon = True
        self._timer = t
        t.start()

    def _on_timer(self):
        # Close if no read happened recently; otherwise keep watching. This is
        # the ONLY recurring wake, and only while the scale is actively in use.
        with self.lock:
            self._timer = None
            if time.time() - self._last_request >= self.IDLE_CLOSE_SECS:
                self._close()
            else:
                self._arm_timer()

    # --- public API ------------------------------------------------------
    # Freshness window for the Android bridge file: no line newer than this ⇒
    # treat the scale as disconnected (unplugged / powered off).
    STALE_MS = 4000

    def _read_from_file(self, path):
        """Android: an app can't open /dev/ttyUSB0, so the native Kotlin bridge
        (UsbScaleBridge, usb-serial-for-android) reads the USB scale and writes the
        freshest line to REDSTARS_SCALE_FILE as '<epoch_ms>\\t<raw>'. Parse it here
        with the SAME SCALE_LINE regex / return shape as the serial path."""
        try:
            with open(path, 'r') as f:
                content = f.read().strip()
        except FileNotFoundError:
            self._last = dict(self._last, connected=False, error='balance bridge: no reading yet')
            return dict(self._last)
        except Exception as e:
            self._last = dict(self._last, connected=False, error=type(e).__name__ + ': ' + str(e))
            return dict(self._last)
        if not content or '\t' not in content:
            self._last = dict(self._last, connected=False, error='balance bridge: no reading yet')
            return dict(self._last)
        ts_str, raw = content.split('\t', 1)
        raw = raw.strip()
        try:
            age_ms = time.time() * 1000.0 - float(ts_str)
        except ValueError:
            age_ms = None
        if age_ms is not None and age_ms > self.STALE_MS:
            self._last = dict(self._last, connected=False, raw=raw, error='scale idle (unplugged?)')
            return dict(self._last)
        m = SCALE_LINE.search(raw)
        if m:
            sign = -1 if m.group('sign') == '-' else 1
            self._last = {
                'connected': True,
                'value': sign * float(m.group('value')),
                'unit': (m.group('unit') or 'g').lower(),
                'sign': m.group('sign'),
                'status': m.group('status'),
                'raw': raw,
                'updated_at': time.time(),
                'error': None,
            }
        else:
            self._last = dict(self._last, connected=True, raw=raw, updated_at=time.time(), error=None)
        return dict(self._last)

    def read(self):
        """Open on demand, drain to the freshest line, return the reading.
        The port auto-closes IDLE_CLOSE_SECS after the last call."""
        _bridge = os.environ.get('REDSTARS_SCALE_FILE')
        if _bridge:
            return self._read_from_file(_bridge)
        with self.lock:
            self._last_request = time.time()
            if self._ser is None:
                try:
                    self._ser = _open_scale_serial(self.port, self.baud)
                except FileNotFoundError:
                    self._last = dict(self._last, connected=False,
                                      error=f'{self.port} not present (scale unplugged?)')
                    return dict(self._last)
                except PermissionError:
                    self._last = dict(self._last, connected=False,
                                      error=f'{self.port} permission denied (udev rule?)')
                    return dict(self._last)
                except Exception as e:
                    self._last = dict(self._last, connected=False,
                                      error=type(e).__name__ + ': ' + str(e))
                    return dict(self._last)
            ser = self._ser
            try:
                latest_raw = None
                latest_m = None
                # Drain everything buffered so we return the FRESHEST line.
                while ser.in_waiting:
                    raw = ser.readline().decode('ascii', errors='replace').strip()
                    if not raw:
                        continue
                    latest_raw = raw
                    m = SCALE_LINE.search(raw)
                    if m:
                        latest_m = m
                if latest_raw is None:
                    # Nothing buffered yet — one short blocking read.
                    raw = ser.readline().decode('ascii', errors='replace').strip()
                    if raw:
                        latest_raw = raw
                        latest_m = SCALE_LINE.search(raw)
            except Exception as e:
                self._close()
                self._last = dict(self._last, connected=False,
                                  error=type(e).__name__ + ': ' + str(e))
                return dict(self._last)

            if latest_m:
                sign = -1 if latest_m.group('sign') == '-' else 1
                self._last = {
                    'connected': True,
                    'value': sign * float(latest_m.group('value')),
                    'unit': (latest_m.group('unit') or 'g').lower(),
                    'sign': latest_m.group('sign'),
                    'status': latest_m.group('status'),
                    'raw': latest_raw,
                    'updated_at': time.time(),
                    'error': None,
                }
            elif latest_raw is not None:
                # Boot message / blank — keep raw for debug, mark connected.
                self._last = dict(self._last, connected=True, raw=latest_raw,
                                  updated_at=time.time(), error=None)
            else:
                # Port open but no data this round — keep last value.
                self._last = dict(self._last, connected=True, error=None)

            if self._timer is None:
                self._arm_timer()
            return dict(self._last)

    # Back-compat alias (old callers used .get()).
    def get(self):
        return self.read()


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

# Refs « par référence » : un répertoire temporaire de symlinks vers des fichiers
# du disque, partagé avec l'utilisateur via xdg-open. Pas de copie des données,
# juste des liens symboliques. Cleanup à la demande via /helper/refs/unmount.
REFS_CACHE_DIR = Path.home() / '.cache' / 'redstars-helper' / 'refs'
REFS = {}  # refs_id → {'dir_path','label','sources':[...],'created_at'}


def _recover_refs_from_disk():
    """Au boot, repeuple REFS depuis les sous-dossiers existants de
    ~/.cache/redstars-helper/refs/<LABEL>-<12hex>. Sans ça, après un
    restart du helper (ou un crash + relaunch via tray), les paths
    d'un mount fait par la session précédente sont rejetés en 403
    'path not under any active /refs/ mount' alors que les symlinks
    existent toujours sur disque. /refs/open et /refs/unmount avec
    l'ID d'un mount pré-restart tombaient aussi en 404."""
    if not REFS_CACHE_DIR.is_dir():
        return
    import re as _re
    pat = _re.compile(r'^(?P<label>.+)-(?P<id>[0-9a-f]{12})$')
    for child in REFS_CACHE_DIR.iterdir():
        if not child.is_dir():
            continue
        m = pat.match(child.name)
        if not m:
            continue
        refs_id = m.group('id')
        if refs_id in REFS:
            continue
        sources = []
        try:
            for entry in child.iterdir():
                try:
                    tgt = os.readlink(str(entry))
                    sources.append(tgt if os.path.isabs(tgt) else str((child / tgt).resolve()))
                except OSError:
                    pass
        except OSError:
            pass
        try:
            created_at = child.stat().st_mtime
        except OSError:
            created_at = time.time()
        REFS[refs_id] = {
            'dir_path': str(child),
            'label': m.group('label'),
            'sources': sources,
            'created_at': created_at,
        }
_recover_refs_from_disk()


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
                dst = src_dir / safe
                # data peut être :
                #   - bytes / bytearray   → write direct
                #   - str / Path qui pointe vers un fichier existant
                #                          → copy stream (films de plusieurs
                #                          Gio sans charger en RAM)
                if isinstance(data, (bytes, bytearray)):
                    dst.write_bytes(bytes(data))
                else:
                    src_path = Path(str(data))
                    if src_path.is_file():
                        shutil.copyfile(src_path, dst)
                    else:
                        # Fallback : on tente bytes()
                        dst.write_bytes(bytes(data))
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


# ─── Self-update over HTTP (triggered by /helper/update) ───────────────
#
# The dashboard polls /api/v1/agents/script-latest for the canonical
# helper.py release, then POSTs /helper/update if the running version
# differs. We fetch + verify minisign + atomic-write + execv ourselves —
# same model as the Tauri shell's auto-update poller, just user-initiated
# from the browser.

# Embedded minisign pubkey — matches the `pubkey` in tauri.conf.json's
# updater config, i.e. the same key that signs the .deb/.dmg/.msi
# updates AND the helper.py release assets (release-script.yml in
# redstars-helper). Verification is fail-closed: missing cryptography
# lib, malformed signature, or signature mismatch → REJECT, keep cache.
_HELPER_MINISIGN_PUBKEY_B64 = 'RWSLOkiWKfscZzD9cOda4UFFRyOZJh5lu/lZZ56+oxa152FXiNtvuM/b'


# Pure-Python Ed25519 verification — embedded so helper.py is fully
# self-contained (no `pip install cryptography` ever). Only used by the
# /helper/update path, so a ~1 s verify on CPython is acceptable.
# Reference: ed25519.cr.yp.to/python/ed25519.py (DJB), trimmed to the
# verify path and switched to iterative scalarmult.
import hashlib as _hashlib

_ED_Q = (1 << 255) - 19
_ED_L = (1 << 252) + 27742317777372353535851937790883648493
_ED_D = (-121665 * pow(121666, _ED_Q - 2, _ED_Q)) % _ED_Q
_ED_I = pow(2, (_ED_Q - 1) // 4, _ED_Q)


def _ed_xrecover(y):
    xx = (y * y - 1) * pow(_ED_D * y * y + 1, _ED_Q - 2, _ED_Q)
    x = pow(xx, (_ED_Q + 3) // 8, _ED_Q)
    if (x * x - xx) % _ED_Q != 0:
        x = (x * _ED_I) % _ED_Q
    if x % 2 != 0:
        x = _ED_Q - x
    return x


_ED_BY = 4 * pow(5, _ED_Q - 2, _ED_Q) % _ED_Q
_ED_BX = _ed_xrecover(_ED_BY)
_ED_B = (_ED_BX % _ED_Q, _ED_BY, 1, (_ED_BX * _ED_BY) % _ED_Q)


def _ed_add(P, Q):
    x1, y1, z1, t1 = P
    x2, y2, z2, t2 = Q
    a = (y1 - x1) * (y2 - x2) % _ED_Q
    b = (y1 + x1) * (y2 + x2) % _ED_Q
    c = t1 * 2 * _ED_D * t2 % _ED_Q
    dd = z1 * 2 * z2 % _ED_Q
    e = (b - a) % _ED_Q
    f = (dd - c) % _ED_Q
    g = (dd + c) % _ED_Q
    h = (b + a) % _ED_Q
    return (e * f % _ED_Q, g * h % _ED_Q, f * g % _ED_Q, e * h % _ED_Q)


def _ed_mult(P, e):
    R = (0, 1, 1, 0)  # identity (extended Edwards coords)
    while e > 0:
        if e & 1:
            R = _ed_add(R, P)
        P = _ed_add(P, P)
        e >>= 1
    return R


def _ed_decode(s):
    y = int.from_bytes(s, 'little') & ((1 << 255) - 1)
    x = _ed_xrecover(y)
    if (x & 1) != ((s[31] >> 7) & 1):
        x = _ED_Q - x
    return (x, y, 1, (x * y) % _ED_Q)


def _ed_encode(P):
    x, y, z, _ = P
    zi = pow(z, _ED_Q - 2, _ED_Q)
    x = (x * zi) % _ED_Q
    y = (y * zi) % _ED_Q
    out = bytearray(y.to_bytes(32, 'little'))
    out[31] |= (x & 1) << 7
    return bytes(out)


def ed25519_verify(pub32, sig64, msg):
    """Pure-Python Ed25519 verify. True iff signature is valid."""
    if len(pub32) != 32 or len(sig64) != 64:
        return False
    try:
        R = _ed_decode(sig64[:32])
        A = _ed_decode(pub32)
        s = int.from_bytes(sig64[32:], 'little')
        h = int.from_bytes(_hashlib.sha512(sig64[:32] + pub32 + msg).digest(),
                           'little') % _ED_L
        return _ed_encode(_ed_mult(_ED_B, s)) == _ed_encode(_ed_add(R, _ed_mult(A, h)))
    except Exception:
        return False


def _verify_minisign(content, sig_text):
    """Verify a minisign signature against the embedded pubkey. Handles
    BOTH algos minisign emits — modern `ED` (Ed25519 over BLAKE2b-512
    prehash, what `minisign -S` produces by default since v0.10) and
    legacy `Ed` (Ed25519 over raw bytes). Self-contained."""
    # Pubkey: base64(algo[2] + key_id[8] + ed25519_pub[32])
    pub_raw = base64.b64decode(_HELPER_MINISIGN_PUBKEY_B64)
    if len(pub_raw) < 42:
        return False
    pub32 = pub_raw[10:42]
    # Signature file: first non-comment base64 line decodes to
    # algo[2] + key_id[8] + ed25519_sig[64]
    sig64 = None
    algo = None
    for line in sig_text.splitlines():
        line = line.strip()
        if not line or line.startswith('untrusted comment') or line.startswith('trusted comment'):
            continue
        try:
            raw = base64.b64decode(line)
            if len(raw) >= 74:
                algo = bytes(raw[:2])
                sig64 = raw[10:74]
                break
        except Exception:
            continue
    if sig64 is None:
        return False
    if algo == b'ED':
        # Modern: signature was made over BLAKE2b-512(content)
        msg = _hashlib.blake2b(content, digest_size=64).digest()
    else:
        # Legacy (b'Ed') or unknown — assume raw signature
        msg = content
    return ed25519_verify(pub32, sig64, msg)


def _user_helper_cache_path() -> Path:
    """Chemin où le Tauri shell lit helper.py EN PRIORITÉ (avant le bundled
    root-owned). Cf. src-tauri/src/script_updater.rs::cache_path et
    AppHandle::app_local_data_dir() :
      - Linux   : $XDG_DATA_HOME/<bundle_id>/helper.py
                  ou ~/.local/share/<bundle_id>/helper.py
      - macOS   : ~/Library/Application Support/<bundle_id>/helper.py
      - Windows : %LOCALAPPDATA%\\<bundle_id>\\helper.py
    bundle_id = 'fr.redlinks.redstars-helper' (tauri.conf.json#identifier)."""
    bundle_id = 'fr.redlinks.redstars-helper'
    sysname = platform.system()
    if sysname == 'Linux':
        base = Path(os.environ.get('XDG_DATA_HOME') or os.path.expanduser('~/.local/share'))
    elif sysname == 'Darwin':
        base = Path.home() / 'Library' / 'Application Support'
    elif sysname == 'Windows':
        base = Path(os.environ.get('LOCALAPPDATA') or os.path.expanduser('~/AppData/Local'))
    else:
        base = Path.home() / '.local' / 'share'
    return base / bundle_id / 'helper.py'


def update_self(api_base='https://api.dev.redstars.redlinks.fr'):
    """Pull the latest signed helper.py from the platform, verify, and
    write it into the USER cache that the Tauri shell loads in priority
    (cf. `_user_helper_cache_path`). On NE TOUCHE PAS le bundled root-owned
    `/usr/lib/Redstars Helper/bundled/helper.py` — le shell est codé pour
    préférer le cache user au bundled, donc écrire là suffit. Returns a
    dict describing the outcome (caller respawns via execv if `updated`
    is True ; au prochain restart du shell le nouveau helper.py est chargé)."""
    import urllib.request, hashlib
    try:
        info_url = api_base.rstrip('/') + '/api/v1/agents/script-latest?name=helper.py'
        with urllib.request.urlopen(info_url, timeout=15) as r:
            info = json.loads(r.read())
    except Exception as e:
        return {'updated': False, 'error': f'fetch script-latest: {e}'}
    new_version = info.get('version', '?')

    # STRICTEMENT plus récent, pas « différent ».
    #
    # Le test était `if new_version == VERSION: skip` — donc toute version DIFFÉRENTE
    # était installée, y compris une plus ANCIENNE. Tant que c'était un bouton qu'un
    # humain pressait, on pouvait vivre avec ; branché sur une boucle automatique, un
    # rollback côté serveur (ou un cache d'API qui traîne) rétrograde silencieusement
    # toutes les machines du parc.
    def _v(x):
        try:
            return tuple(int(p) for p in str(x).strip().split('.'))
        except (TypeError, ValueError):
            return ()
    cur, new = _v(VERSION), _v(new_version)
    if not new or not cur:
        return {'updated': False, 'version': VERSION,
                'error': f'version illisible: {new_version!r} / {VERSION!r}'}
    if new <= cur:
        return {'updated': False, 'version': VERSION,
                'reason': 'already up to date' if new == cur else f'refus de rétrograder vers {new_version}'}
    try:
        with urllib.request.urlopen(info['script_url'], timeout=30) as r:
            script_bytes = r.read()
        with urllib.request.urlopen(info['signature_url'], timeout=15) as r:
            sig_text = r.read().decode('utf-8', errors='replace')
    except Exception as e:
        return {'updated': False, 'error': f'fetch release asset: {e}'}
    expected = info.get('sha256', '')
    if expected:
        got = hashlib.sha256(script_bytes).hexdigest()
        if got != expected:
            return {'updated': False, 'error': f'sha256 mismatch: expected {expected[:16]}…, got {got[:16]}…'}
    if not _verify_minisign(script_bytes, sig_text):
        return {'updated': False, 'error': 'minisign verification failed (or cryptography lib missing)'}
    # On VISE le cache user, jamais le bundled root-owned. Le Tauri shell
    # lit cache user > bundled dans script_updater.rs (resolution order).
    # Le helper.py qui tourne là maintenant continue de tourner ; l'update
    # prend effet au prochain restart du shell.
    target = _user_helper_cache_path()
    try:
        target.parent.mkdir(parents=True, exist_ok=True)
    except Exception as e:
        return {'updated': False, 'error': f'mkdir {target.parent}: {e}'}
    if not os.access(target.parent, os.W_OK):
        return {'updated': False, 'error': f'cannot write to {target.parent}'}
    tmp = target.with_suffix('.py.tmp')
    try:
        tmp.write_bytes(script_bytes)
        os.replace(tmp, target)
        # Le shell utilise ce marker pour afficher la version chargée
        # dans le statut + skipper le fetch si déjà à jour au prochain boot.
        (target.parent / 'helper.version').write_text(new_version)
    except Exception as e:
        return {'updated': False, 'error': f'write failed: {e}'}
    return {
        'updated': True,
        'from_version': VERSION,
        'version': new_version,
        'path': str(target),
        'size': len(script_bytes),
    }


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


# ─── Arrêt quand le site est fermé ─────────────────────────────────────────
# Un site ouvert envoie GET /helper/ping toutes les 4 min (HelperHeartbeat,
# monté dans le layout racine du frontend) et n'importe quelle autre requête
# compte aussi. Sans nouvelles pendant IDLE_EXIT_S, le helper s'arrête — shell
# compris — pour ne pas garder de mémoire pour un site que personne n'a ouvert.
# Rouvrir le site le relance : AgentStatus → redstars-helper://launch.
#
# 100 % local : le battement vient du navigateur de cette machine, vers
# 127.0.0.1. Rien ne sort, et la réponse fait 12 octets.
#
# 12 min = trois battements. Les navigateurs ralentissent les minuteries des
# onglets en arrière-plan (Chrome les aligne sur la minute) : deux battements
# manqués ne doivent pas suffire à couper un helper dont le site est ouvert.
#
# Horloge monotone : sur Linux elle ne compte pas la veille, donc un portable
# qui se réveille ne trouve pas son helper « muet depuis 8 h ».
#
# Désactivé sur Android (le helper vit dans l'appli, qui EST le site) ;
# désactivable partout avec REDSTARS_HELPER_IDLE_EXIT_S=0.
IDLE_EXIT_S = int(os.environ.get('REDSTARS_HELPER_IDLE_EXIT_S', '720'))
_last_seen = time.monotonic()


def _mark_seen():
    global _last_seen
    _last_seen = time.monotonic()   # affectation d'un float : atomique sous CPython


# Nom du binaire Tauri. Sur Linux, /proc/<pid>/comm est tronqué à 15 caractères
# — « redstars-helper » en fait exactement 15.
_SHELL_NAMES = ('redstars-helper', 'redstars helper')


def _is_helper_shell(pid):
    """Le processus `pid` est-il le shell Tauri du helper ? Faux au moindre doute."""
    try:
        if sys.platform.startswith('linux'):
            return Path(f'/proc/{pid}/comm').read_text().strip().lower() in _SHELL_NAMES
        if sys.platform == 'darwin':
            out = subprocess.run(['ps', '-o', 'comm=', '-p', str(pid)],
                                 capture_output=True, text=True, timeout=5).stdout.strip()
            return os.path.basename(out).lower() in _SHELL_NAMES
        if os.name == 'nt':
            out = subprocess.run(['tasklist', '/FI', f'PID eq {pid}', '/FO', 'CSV', '/NH'],
                                 capture_output=True, text=True, timeout=5).stdout.lower()
            return any(n in out for n in _SHELL_NAMES)
    except Exception:
        pass
    return False


def _stop_parent_shell():
    """Arrête le shell Tauri qui nous a lancés.

    Sans ça, lui reste en mémoire : il ne surveille pas son enfant Python. Et
    c'est d'ici qu'il faut le faire, pas du shell : le rpm/deb ne se met pas à
    jour tout seul, alors que ce fichier, si (canal script-py). Seulement si le
    parent est VRAIMENT le shell — lancé à la main depuis un terminal, le parent
    est le shell de l'utilisateur, et on n'y touche pas."""
    ppid = os.getppid()
    if ppid <= 1 or not _is_helper_shell(ppid):
        return
    try:
        if os.name == 'nt':
            subprocess.run(['taskkill', '/PID', str(ppid), '/F'], capture_output=True, timeout=5)
            return
        import signal
        os.kill(ppid, signal.SIGTERM)
        # Un parent qui meurt nous fait adopter AU MOMENT de sa sortie, zombie ou
        # pas : getppid() qui change est donc la preuve qu'il est parti — et
        # évite de viser un PID recyclé entre-temps.
        for _ in range(30):
            time.sleep(0.1)
            if os.getppid() != ppid:
                return
        os.kill(ppid, signal.SIGKILL)
    except Exception as e:
        print(f'  arrêt du shell impossible : {e}', flush=True)


def _start_idle_watchdog():
    if os.environ.get('REDSTARS_HELPER_PLATFORM') == 'android':
        return   # le helper vit dans l'appli, qui est le site : il meurt avec elle
    if IDLE_EXIT_S <= 0:
        print('  arrêt sur inactivité : désactivé (REDSTARS_HELPER_IDLE_EXIT_S=0)')
        return
    span = f'{IDLE_EXIT_S // 60} min' if IDLE_EXIT_S >= 120 else f'{IDLE_EXIT_S} s'

    def _loop():
        while True:
            left = IDLE_EXIT_S - (time.monotonic() - _last_seen)
            if left <= 0:
                break
            # Dormir jusqu'à l'échéance, pas plus souvent : un réveil par
            # battement au pire, et la file d'attente du noyau fait le reste.
            time.sleep(max(5.0, left))
        print(f'redstars-helper: aucun signe du site depuis {span} — arrêt.', flush=True)
        _stop_parent_shell()
        os._exit(0)

    threading.Thread(target=_loop, name='idle-watchdog', daemon=True).start()
    print(f'  arrêt sur inactivité : après {span} sans nouvelles du site')


class Handler(BaseHTTPRequestHandler):
    """The /helper/* API, and nothing else — no static files since 0.5.64."""


    def end_headers(self):
        # Allow CORS for /helper/* from the dev/prod dashboards. Same-origin
        # callers (page served by helper itself) get the headers too — no harm.
        origin = self.headers.get('Origin', '')
        if origin in ALLOWED_ORIGINS or self.path.startswith('/helper/'):
            allow = origin if origin in ALLOWED_ORIGINS else '*'
            self.send_header('Access-Control-Allow-Origin', allow)
            self.send_header('Access-Control-Allow-Methods', 'GET, POST, OPTIONS')
            self.send_header('Access-Control-Allow-Headers', 'Content-Type')
            # Private Network Access (W3C PNA spec) : Firefox 142+/Chrome 130+
            # exigent ce header dans la réponse au preflight quand une page
            # publique (https://dev.redstars.redlinks.fr) cible une IP
            # privée (127.0.0.1 via local.redlinks.fr). Sans → Firefox
            # bloque silencieusement avec "Status code: (null)".
            self.send_header('Access-Control-Allow-Private-Network', 'true')
        # Cross-origin isolation for future SharedArrayBuffer / WASM threads.
        self.send_header('Cross-Origin-Opener-Policy', 'same-origin')
        self.send_header('Cross-Origin-Embedder-Policy', 'require-corp')
        self.send_header('Cross-Origin-Resource-Policy', 'cross-origin')
        super().end_headers()

    def do_OPTIONS(self):
        # Preflight for the dashboard's cross-origin /helper/* calls.
        _mark_seen()
        self.send_response(204)
        self.end_headers()

    def _json(self, code, payload):
        body = json.dumps(payload).encode()
        self.send_response(code)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _raw(self, code, body):
        self.send_response(code)
        self.send_header('Content-Type', 'application/octet-stream')
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
        _mark_seen()   # toute requête prouve qu'un client est là
        if not self.path.startswith('/helper/'):
            # No static files any more: the demo page and everything next to this
            # script (keys included) used to be served from here.
            self._json(404, {'error': 'not found'})
            return
        split = urlsplit(self.path)
        ep = split.path[len('/helper'):]  # strip prefix → /status, /lsusb, etc.
        query = parse_qs(split.query)
        if ep == '/ping':
            # Battement du site ouvert (HelperHeartbeat). _mark_seen() en tête
            # de do_GET a déjà fait le travail : on répond le strict minimum.
            self._json(200, {'ok': True})
            return
        if ep == '/status':
            self._json(200, {'ok': True, 'version': VERSION})
            return
        if ep == '/disk':
            # Espace dispo sur la partition qui hébergera les sorties.
            # ?path=<…> ou défaut = le cache redstars-helper (= là où
            # /refs/ écrit).
            target = (query.get('path', ['']) or [''])[0]
            if not target:
                target = str(Path(os.environ.get('XDG_CACHE_HOME')
                                  or os.path.expanduser('~/.cache')) / 'redstars-helper')
            try:
                Path(target).mkdir(parents=True, exist_ok=True)
                usage = shutil.disk_usage(target)
                self._json(200, {
                    'ok': True, 'path': target,
                    'total': usage.total, 'free': usage.free, 'used': usage.used,
                })
            except Exception as e:
                self._json(500, {'error': f'{type(e).__name__}: {e}'})
            return
        if ep == '/scale':
            state = SCALE.read()
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

        if ep == '/files/pick':
            # Picker natif multi-fichiers OU multi-dossiers via zenity/kdialog.
            # Renvoie la liste des paths choisis sur le disque (sans copier
            # les données).
            #   ?mode=files (défaut)  → fichiers multi
            #   ?mode=dirs            → dossiers multi (kdialog : 1 seul)
            mode = (query.get('mode', ['files']) or ['files'])[0].lower()
            if mode == 'dirs':
                tools = [
                    ['zenity', '--file-selection', '--directory', '--multiple', '--separator=\n'],
                    ['kdialog', '--getexistingdirectory', os.path.expanduser('~')],
                ]
            else:
                mode = 'files'
                tools = [
                    ['zenity', '--file-selection', '--multiple', '--separator=\n'],
                    ['kdialog', '--multiple', '--getopenfilename', os.path.expanduser('~')],
                ]
            paths = None
            err = None
            for cmd in tools:
                if shutil.which(cmd[0]) is None:
                    continue
                try:
                    out = subprocess.run(cmd, capture_output=True, text=True, timeout=300)
                    if out.returncode != 0:
                        # User cancelled — return empty list, not an error
                        paths = []
                        break
                    sep = '\n' if cmd[0] == 'zenity' else ' '
                    paths = [p for p in out.stdout.strip().split(sep) if p]
                    break
                except subprocess.TimeoutExpired:
                    err = f'{cmd[0]} timeout'
                except Exception as e:
                    err = f'{type(e).__name__}: {e}'
            if paths is None:
                self._json(500, {'error': err or 'no native picker (install zenity or kdialog)'})
                return
            # enrichir avec taille + nom pour l'UI
            entries = []
            for p in paths:
                pth = Path(p)
                try:
                    st = pth.stat()
                    entries.append({'name': pth.name, 'path': str(pth), 'size': st.st_size, 'is_dir': pth.is_dir()})
                except OSError:
                    entries.append({'name': pth.name, 'path': str(pth), 'size': 0, 'is_dir': False, 'missing': True})
            self._json(200, {'paths': paths, 'entries': entries})
            return

        if ep == '/refs/list':
            refs_id = (query.get('id', ['']) or [''])[0]
            info = REFS.get(refs_id)
            if not info:
                self._json(404, {'error': 'unknown id'}); return
            self._json(200, {
                'id': refs_id,
                'mount_path': info['dir_path'],
                'label': info['label'],
                'entries': list_mount(info['dir_path']),
            })
            return

        self._json(404, {'error': 'unknown helper endpoint'})

    def do_HEAD(self):
        if not self.path.startswith('/helper/'):
            # No static files any more (see do_GET). This used to fall through to
            # SimpleHTTPRequestHandler; with the plain base class it would raise.
            self.send_response(404)
            self.end_headers()
            return
        # Helper endpoints don't really do HEAD, just say OK.
        self.send_response(200)
        self.end_headers()

    def do_POST(self):
        _mark_seen()
        if not self.path.startswith('/helper/'):
            self._json(404, {'error': 'POST only allowed under /helper/*'})
            return
        split = urlsplit(self.path)
        ep    = split.path[len('/helper'):]
        if ep == '/session':
            # Conserver la session, SANS ouvrir de terminal.
            #
            # Un bouton « ouvrir en console » ne suffit pas : il faut y penser. Alors
            # que le tableau de bord, lui, est ouvert. Tant qu'on est connecté au web,
            # le helper doit avoir une session fraîche — et `minitel` tapé dans un
            # terminal, trois jours plus tard, marche sans rien demander.
            #
            # Même exigence d'origine que /console : cette route n'ouvre rien, mais elle
            # ÉCRIT un secret sur le disque, et le CORS permissif du reste de /helper/*
            # laisserait n'importe quel site nous refiler le sien.
            origin = self.headers.get('Origin', '')
            if origin not in ALLOWED_ORIGINS:
                self._json(403, {'error': f'origine refusée: {origin or "(absente)"}'})
                return
            try:
                b = self._read_json()
                token = (b.get('token') or '').strip()
                refresh = (b.get('refresh') or '').strip()
                if not token or not refresh:
                    # Sans refresh, il n'y a rien à conserver qui vaille : un jeton
                    # d'accès seul est mort dans quinze minutes.
                    self._json(400, {'error': 'jeton et refresh requis'}); return
                _con_session_save(token, refresh, (b.get('api_url') or DEFAULT_API_URL).strip())
                self._json(200, {'ok': True})       # jamais d'écho du jeton
            except Exception as e:
                self._json(500, {'error': f'{type(e).__name__}: {e}'})
            return
        if ep == '/console':
            # Ouvrir un terminal est d'une tout autre gravité que lire une balance.
            # `end_headers` accorde `Access-Control-Allow-Origin: *` à tout
            # /helper/* dont l'origine est inconnue — c'est-à-dire que N'IMPORTE
            # QUEL site peut appeler nos routes. On ne s'appuie donc PAS dessus
            # ici : cette route exige explicitement une origine de la liste, et
            # refuse tout le reste. Le CORS dit au navigateur ce qu'il a le droit
            # de lire ; il ne dit rien à un client qui n'est pas un navigateur.
            origin = self.headers.get('Origin', '')
            if origin not in ALLOWED_ORIGINS:
                self._json(403, {'error': f'origine refusée: {origin or "(absente)"}'})
                return
            try:
                b = self._read_json()
                token = (b.get('token') or '').strip()
                if not token:
                    self._json(400, {'error': 'jeton manquant'}); return

                # Le navigateur nous passe AUSSI de quoi renouveler. Sans ça, la session
                # qu'il vient de nous offrir est morte dans un quart d'heure et la
                # prochaine console redemande le mot de passe — ce qui est exactement
                # l'ennui qu'on prétendait supprimer.
                refresh = (b.get('refresh') or '').strip()
                api = (b.get('api_url') or DEFAULT_API_URL).strip()
                if refresh:
                    try:
                        _con_session_save(token, refresh, api)
                    except OSError as e:
                        print(f"[helper] session non conservée : {e}", file=sys.stderr)

                term = _con_spawn_terminal(token, b)
                if not term:
                    self._json(503, {'error': "aucun émulateur de terminal trouvé — définissez $TERMINAL"})
                    return
                # On ne renvoie JAMAIS le jeton, même en écho de la requête.
                self._json(200, {'ok': True, 'terminal': term})
            except Exception as e:
                self._json(500, {'error': f'{type(e).__name__}: {e}'})
            return
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

        if ep == '/update':
            body = self._read_json()
            api_base = body.get('api_base') or 'https://api.dev.redstars.redlinks.fr'
            try:
                result = update_self(api_base)
            except Exception as e:
                self._json(500, {'error': type(e).__name__ + ': ' + str(e)})
                return
            self._json(200, result)
            # If we replaced our own file, exec the same interpreter on the
            # same path so we come back up with the new code. Brief sleep so
            # the response gets flushed first.
            if result.get('updated'):
                import sys as _sys
                def _restart():
                    time.sleep(0.5)
                    try:
                        os.execv(_sys.executable, [_sys.executable, str(Path(__file__).resolve())])
                    except Exception as e:
                        print(f'[update] execv failed: {e}')
                threading.Thread(target=_restart, daemon=True).start()
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


        if ep == '/refs/mount':
            body  = self._read_json()
            paths = body.get('paths') or []
            label = (body.get('label') or 'REDSTARS').strip() or 'REDSTARS'
            if not paths:
                self._json(400, {'error': 'paths required'}); return
            # valider que chaque path existe AVANT de créer quoi que ce soit
            missing = [p for p in paths if not Path(p).exists()]
            if missing:
                self._json(400, {'error': 'path not found', 'missing': missing}); return
            REFS_CACHE_DIR.mkdir(parents=True, exist_ok=True)
            refs_id = uuid.uuid4().hex[:12]
            dir_path = REFS_CACHE_DIR / f'{label}-{refs_id}'
            try:
                dir_path.mkdir(parents=True, exist_ok=False)
            except FileExistsError:
                self._json(500, {'error': 'tmp dir collision'}); return
            entries = []
            for p in paths:
                src = Path(p).resolve()
                # éviter les collisions de nom : suffixer si nécessaire
                base = src.name
                target = dir_path / base
                i = 1
                while target.exists():
                    stem = src.stem; ext = src.suffix
                    target = dir_path / f'{stem}-{i}{ext}'
                    i += 1
                try:
                    target.symlink_to(src)
                    st = src.stat()
                    entries.append({
                        'name': target.name, 'path': str(target),
                        'target': str(src), 'size': st.st_size,
                        'is_dir': src.is_dir(),
                    })
                except OSError as e:
                    self._json(500, {'error': f'symlink failed: {e}', 'path': str(src)}); return
            REFS[refs_id] = {
                'dir_path': str(dir_path),
                'label': label,
                'sources': [str(Path(p).resolve()) for p in paths],
                'created_at': time.time(),
            }
            self._json(200, {
                'id': refs_id,
                'mount_path': str(dir_path),
                'label': label,
                'entries': entries,
            })
            return

        if ep == '/refs/add':
            # Ajoute des symlinks à un mount /refs/ existant — pour pouvoir
            # cumuler plusieurs sélections (fichiers + dossiers) tant que
            # le user n'a pas cliqué Clore. La sortie est juste la liste
            # des entrées ajoutées (le caller les append dans son state).
            body = self._read_json()
            refs_id   = body.get('id', '')
            new_paths = body.get('paths') or []
            info = REFS.get(refs_id)
            if not info:
                self._json(404, {'error': 'unknown id'}); return
            if not new_paths:
                self._json(400, {'error': 'paths required'}); return
            dir_path = Path(info['dir_path'])
            if not dir_path.is_dir():
                self._json(500, {'error': 'mount dir disappeared'}); return
            added = []
            for p in new_paths:
                src = Path(p)
                if not src.exists():
                    added.append({
                        'name': src.name, 'path': str(dir_path / src.name),
                        'target': str(src), 'size': 0, 'is_dir': False, 'missing': True,
                    })
                    continue
                src_res = src.resolve()
                # Évite les collisions de nom comme dans /refs/mount.
                base   = src_res.name
                target = dir_path / base
                i = 1
                while target.exists():
                    stem = src_res.stem; ext = src_res.suffix
                    target = dir_path / f'{stem}-{i}{ext}'
                    i += 1
                try:
                    target.symlink_to(src_res)
                    st = src_res.stat()
                    added.append({
                        'name': target.name, 'path': str(target),
                        'target': str(src_res), 'size': st.st_size,
                        'is_dir': src_res.is_dir(),
                    })
                except OSError as e:
                    self._json(500, {'error': f'symlink failed: {e}', 'path': str(src_res)}); return
            info['sources'].extend(str(Path(p).resolve()) for p in new_paths)
            self._json(200, {'ok': True, 'added': added})
            return

        if ep == '/refs/open':
            body = self._read_json()
            refs_id = body.get('id', '')
            info = REFS.get(refs_id)
            if not info:
                # Fallback : depuis 0.5.18 le decode et le bundle save
                # construisent une vraie iso via make_iso → l'entry vit
                # dans MOUNTED, pas REFS. Aligne la forme attendue par la
                # suite (dir_path).
                m = MOUNTED.get(refs_id)
                if m:
                    info = {'dir_path': m['mount_path'], 'label': m.get('label', '')}
                else:
                    self._json(404, {'error': 'unknown id'}); return
            target = body.get('path') or info['dir_path']
            # Path must sit under the refs dir we created. We use absolute() +
            # normpath, NOT resolve() — the refs are symlinks pointing OUTSIDE
            # the tmpdir (their whole purpose). resolve() would follow them
            # and the check would always fail. We trust the symlinks we wrote
            # at mount time; what we're guarding against is the caller asking
            # to open `.../../../etc/passwd` literally, which normpath catches.
            mount_norm  = os.path.normpath(os.path.abspath(info['dir_path']))
            target_norm = os.path.normpath(os.path.abspath(target))
            if target_norm != mount_norm and not target_norm.startswith(mount_norm + os.sep):
                self._json(403, {'error': 'path not in refs mount'}); return
            # Pick the right opener. Symlinks to a directory must land
            # in a file manager, NOT xdg-open — VSCode/VSCodium and
            # other editors register themselves as inode/directory
            # handlers ("Open Folder…"), which hijacks the mount and
            # the user never sees their files in a real explorer. For
            # plain files we keep xdg-open: the user's MIME defaults
            # are their choice (e.g. VSCodium for text/plain is fine).
            # Pick the opener. For DIRECTORIES we MUST bypass xdg-open
            # because editors (VSCodium, VSCode, Cursor, IntelliJ, …)
            # register themselves as `inode/directory` handlers via
            # their .desktop file ("Open Folder…"), and on many distros
            # they end up as the default. xdg-open then hands the mount
            # to the editor and the user never sees it in a real file
            # manager. For plain FILES we keep xdg-open : the user's
            # MIME defaults are their choice.
            #
            # FILE_MANAGERS_LINUX is intentionally large : every major
            # FM (GTK/Qt/KDE/MATE/Cinnamon/XFCE/LXQt/UKUI/Deepin/…)
            # ships a single well-known binary, we try them in order
            # and pick the first present. Order matters : a user who
            # has both Thunar and Nautilus installed gets the one that
            # ships with their session DE (XDG_CURRENT_DESKTOP hint).
            FILE_MANAGERS_LINUX = [
                'thunar', 'nautilus', 'dolphin', 'nemo', 'caja',
                'pcmanfm-qt', 'pcmanfm', 'peony', 'konqueror',
                'krusader', 'spacefm', 'index.fm', 'qtfm',
                'dde-file-manager', 'cosmic-files', 'nautilus-desktop',
                'gnome-files', 'Files',
            ]
            sysname = platform.system()
            try:
                is_dir = os.path.isdir(target)  # follows symlinks
                opener_label = 'xdg'
                if sysname == 'Linux' or sysname.endswith('BSD'):
                    cmd = None
                    if is_dir:
                        # Bias toward the FM bundled with the current
                        # session DE so a Thunar+Nautilus box on XFCE
                        # gets Thunar, on GNOME gets Nautilus, etc.
                        dt = (os.environ.get('XDG_CURRENT_DESKTOP', '') + ':'
                              + os.environ.get('XDG_SESSION_DESKTOP', '')).lower()
                        de_map = {'xfce': 'thunar', 'gnome': 'nautilus',
                                  'kde': 'dolphin', 'plasma': 'dolphin',
                                  'mate': 'caja', 'cinnamon': 'nemo',
                                  'lxqt': 'pcmanfm-qt', 'lxde': 'pcmanfm',
                                  'ukui': 'peony', 'deepin': 'dde-file-manager',
                                  'cosmic': 'cosmic-files'}
                        preferred = next((fm for tok, fm in de_map.items() if tok in dt), None)
                        order = ([preferred] if preferred else []) + \
                                [f for f in FILE_MANAGERS_LINUX if f != preferred]
                        for fm in order:
                            if shutil.which(fm):
                                cmd = [fm, target]
                                opener_label = f'fm:{fm}'
                                break
                    if cmd is None:
                        cmd = ['xdg-open', target]
                    subprocess.Popen(cmd)
                elif sysname == 'Darwin':
                    # macOS `open <dir>` opens Finder on that path.
                    # No editor hijack issue : Finder always wins for
                    # bare `open` on directories.
                    subprocess.Popen(['open', target])
                    opener_label = 'finder' if is_dir else 'xdg'
                elif sysname == 'Windows':
                    # Windows `explorer.exe <dir>` opens File Explorer.
                    # os.startfile on a directory normally does the
                    # same but explicit explorer is more predictable.
                    if is_dir:
                        subprocess.Popen(['explorer', target])
                        opener_label = 'explorer'
                    else:
                        os.startfile(target)  # type: ignore[attr-defined]
                else:
                    self._json(500, {'error': f'open unsupported on {sysname}'}); return
                self._json(200, {'ok': True, 'path': target, 'opener': opener_label})
            except Exception as e:
                self._json(500, {'error': f'{type(e).__name__}: {e}'})
            return

        if ep == '/refs/unmount':
            body = self._read_json()
            refs_id = body.get('id', '')
            info = REFS.pop(refs_id, None)
            if not info:
                self._json(404, {'error': 'unknown id'}); return
            try:
                # supprimer les symlinks puis le répertoire ; ne touche pas aux fichiers cibles
                d = Path(info['dir_path'])
                if d.exists():
                    for child in d.iterdir():
                        try: child.unlink()
                        except OSError: pass
                    d.rmdir()
            except OSError as e:
                self._json(500, {'error': f'unmount failed: {e}', 'id': refs_id, 'dir': info['dir_path']}); return
            self._json(200, {'ok': True, 'id': refs_id})
            return

        self._json(404, {'error': 'unknown helper endpoint'})

    def log_message(self, fmt, *args):
        # Silent — uncomment to debug
        pass


def _serve_thread(server, label):
    print(f'  {label}: ready')
    try:
        # poll_interval=30 (default 0.5): serve_forever wakes only to re-check
        # an internal shutdown flag we never set (the helper exits via signal /
        # process kill, not server.shutdown()). A long interval keeps the agent
        # dormant — a few wakeups per minute instead of ~2/s — when idle.
        server.serve_forever(poll_interval=30)
    except Exception as e:
        print(f'  {label} crashed: {e}')


# ============================================================================
# RedStars en console — le client terminal, replié dans helper.py
# ============================================================================
#
#     python3 helper.py console --user X --app eau
#     python3 helper.py console --app eau --minitel      (40×24)
#     python3 helper.py console --app eau --accessible   (liste numérotée)
#
# Pourquoi ICI et pas dans un fichier à côté : `helper.py` est le SEUL fichier
# que la release `script-py-v*` embarque, donc le seul qui s'auto-met à jour.
# Un client console dans son propre module aurait été prisonnier d'un rebuild
# complet du rpm à chaque correction — exactement ce que la décision « un seul
# helper.py auto-mis-à-jour » existe pour éviter.
#
# Le client est DÉLIBÉRÉMENT BÊTE : il ne sait ni ce qu'est une app, ni un slot,
# ni une colonne. Il envoie « je fais N colonnes, je suis ici, on a appuyé sur
# ↓ » et imprime ce qu'on lui renvoie. Tout le savoir vit dans le core, à côté
# des renderers React qui lisent les MÊMES descripteurs. Le jour où ce bloc
# contient un `if app == "eau"`, l'architecture a échoué.
# ============================================================================

import argparse
import base64
import getpass
import sys
import termios
import time
import tty
import urllib.error
import urllib.parse
import urllib.request

DEFAULT_APP_URL = "https://dev.redstars.redlinks.fr"
DEFAULT_API_URL = "https://api.dev.redstars.redlinks.fr"

_CON_CTX = ssl.create_default_context()


def _con_req(url, data=None, headers=None, method=None, raw=False):
    body = json.dumps(data).encode() if data is not None else None
    h = {"Content-Type": "application/json", **(headers or {})}
    r = urllib.request.Request(url, data=body, headers=h, method=method or ("POST" if body else "GET"))
    with urllib.request.urlopen(r, context=_CON_CTX, timeout=30) as resp:
        payload = resp.read()
    return payload if raw else json.loads(payload or b"{}")


# ── Auth + org ──────────────────────────────────────────────────────────────

def _con_login(api, username, password):
    d = _con_req(f"{api}/api/v1/auth/login", {"username": username, "password": password})
    return d["access_token"]


def _con_orgs(api, token):
    d = _con_req(f"{api}/api/v1/organizations", headers={"Authorization": f"Bearer {token}"})
    return d if isinstance(d, list) else d.get("organizations", d.get("data", []))


# ── Le moteur : on ne fait que DEMANDER une trame ───────────────────────────

# Profondeur de couleur négociée avec le terminal. Les images d'icônes portent
# désormais leur RGB EXACT : un terminal truecolor l'affiche tel quel — identique au
# Web, sans le banding de la palette 256. On ne DEMANDE truecolor que si le terminal
# l'annonce (COLORTERM), sinon 256 couleurs, la valeur sûre partout. Le Minitel (vraie
# console Linux + setfont) reste en 256 : le noyau ne rend pas le truecolor de toute façon.
_CON_COLOR = "ansi256"
_CON_IMG = False       # ce client sait-il décoder les images nn: (négocié au démarrage)

def _con_color_depth(minitel):
    if minitel:
        return "ansi256"
    if os.environ.get("COLORTERM", "").lower() in ("truecolor", "24bit"):
        return "truecolor"
    return "ansi256"

def _con_frame(app_url, token, **q):
    # On n'envoie pas ce qu'on n'a pas. Sans ce filtre, `app=None` part sur le réseau
    # comme la chaîne "None" — et le moteur cherche consciencieusement une application
    # qui s'appelle None. Omettre `app`, c'est demander l'ACCUEIL.
    q = {k: v for k, v in q.items() if v is not None}

    # Le fuseau, en minutes à l'est de UTC. Le moteur tourne côté serveur, en UTC ; un
    # terminal n'a pas de fuseau navigateur à lui offrir. Sans ça, l'agenda affiche un
    # événement de 15h30 à 13h30 — pas une heure arrondie, une heure FAUSSE. Le client,
    # lui, tourne sur la machine de l'utilisateur : il connaît son décalage, il l'envoie.
    if "tz" not in q:
        off = time.localtime().tm_gmtoff        # secondes à l'est de UTC (None si inconnu)
        if off is not None:
            q["tz"] = off // 60
    # La profondeur de couleur voyage avec la trame : le moteur émet le RGB exact des
    # images quand on demande truecolor (rendu Web), la palette 256 sinon.
    if "color" not in q:
        q["color"] = _CON_COLOR
    # Capacité image : on n'annonce `img=1` que si ce client sait décoder les nn: hashes,
    # pour que le serveur ne réserve d'espace-avatar que là où on saura le remplir.
    if "img" not in q and _CON_IMG:
        q["img"] = 1
    url = f"{app_url}/api/console/frame/?" + urllib.parse.urlencode(q)
    try:
        return _con_req(url, headers={"Authorization": f"Bearer {token}"})
    except urllib.error.HTTPError as e:
        return {"error": json.loads(e.read() or b"{}").get("error", f"HTTP {e.code}")}


# ── La session, conservée ────────────────────────────────────────────────────
#
# Le jeton d'accès vit QUINZE MINUTES. Celui que le navigateur nous passe est donc
# périmé avant qu'on ait fini de lire l'écran d'accueil, et relancer la console
# depuis la barre système redemandait le mot de passe à chaque fois — dans un
# terminal, sans gestionnaire de mots de passe, en aveugle. C'est précisément la
# friction que tout ce travail existe pour supprimer, et elle était intacte.
#
# Le refresh_token, lui, vit SEPT JOURS. On garde donc les deux : on se connecte une
# fois dans le navigateur, et la console marche pendant une semaine sans rien taper.
#
# Le fichier est en 0600, dans le répertoire d'état de l'utilisateur. C'est la même
# exposition qu'un jeton dans /proc/<pid>/environ — le propriétaire et root — et la
# même que celle du trousseau du navigateur juste à côté. Ce qu'on n'accepte pas,
# c'est argv, que TOUS les utilisateurs de la machine peuvent lire.

def _con_session_path():
    base = os.environ.get("XDG_STATE_HOME") or os.path.expanduser("~/.local/state")
    d = os.path.join(base, "redstars")
    os.makedirs(d, mode=0o700, exist_ok=True)
    return os.path.join(d, "session.json")


def _con_session_save(access, refresh, api):
    """Écrit la session. On ne la garde QUE si on a de quoi la renouveler : un jeton
    d'accès seul serait mort dans un quart d'heure, et le stocker ne ferait que
    laisser traîner un secret sans rien résoudre."""
    if not refresh:
        return
    path = _con_session_path()
    tmp = path + ".tmp"
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w") as f:
        json.dump({"access_token": access, "refresh_token": refresh, "api": api}, f)
    os.replace(tmp, path)          # atomique : jamais de fichier à moitié écrit


def _con_jwt_expired(tok, margin=60):
    """Le jeton est-il périmé (ou sur le point de l'être) ?

    On lit le `exp` du JWT plutôt que d'attendre un 401 : partir sur un jeton mort,
    c'est un aller-retour réseau perdu et un message d'erreur que l'utilisateur n'a
    pas à voir. La marge évite d'expirer PENDANT la requête."""
    try:
        payload = tok.split(".")[1]
        payload += "=" * (-len(payload) % 4)
        exp = json.loads(base64.urlsafe_b64decode(payload)).get("exp", 0)
        return time.time() + margin >= exp
    except Exception:
        return True                # illisible = à jeter


def _con_session_load(api):
    """Rend un jeton d'accès VALIDE, ou None. Renouvelle en silence si besoin."""
    try:
        with open(_con_session_path()) as f:
            d = json.load(f)
    except (OSError, ValueError):
        return None

    access, refresh = d.get("access_token"), d.get("refresh_token")
    if access and not _con_jwt_expired(access):
        return access
    if not refresh:
        return None

    # Le refresh passe par un EN-TÊTE, pas par le corps. Le mettre dans le corps
    # renvoie « Refresh token required », ce qui ressemble à un jeton absent alors
    # que c'est un jeton mal adressé.
    try:
        r = urllib.request.Request(
            f"{api}/api/v1/auth/refresh",
            headers={"Content-Type": "application/json", "X-Refresh-Token": refresh},
            method="POST")
        with urllib.request.urlopen(r, context=_CON_CTX, timeout=15) as resp:
            new = json.loads(resp.read() or b"{}")
    except Exception:
        return None                # refresh mort ou hors ligne : on redemandera

    access = new.get("access_token")
    if not access:
        return None
    # Rotation : le serveur PEUT renvoyer un nouveau refresh. Garder l'ancien alors
    # qu'il vient d'être révoqué, c'est se déconnecter tout seul dans sept jours.
    _con_session_save(access, new.get("refresh_token") or refresh, api)
    return access


# ── Clavier ─────────────────────────────────────────────────────────────────

def _con_read_key(fd):
    """Une touche → une intention. Les mêmes intentions que la trame annonce
    dans ses `hints`, donc la même sémantique qu'un ENVOI/SUITE de Minitel.

    F1 est traité à part : c'est la touche qui bascule vers le mode accessibilité.
    Deux terminaux, deux encodages — ESC O P (VT100) et ESC [ 1 1 ~ (xterm récent) —
    et les rater ferait de F1 un « Retour », c'est-à-dire une sortie surprise."""
    c = os.read(fd, 1)
    if c == b"\x1b":                       # séquence CSI/SS3, ou Échap seul
        seq = os.read(fd, 2)
        if seq == b"OP":                   # F1, VT100/SS3
            return "a11y"
        if seq == b"[1":                   # peut être F1 en xterm : ESC [ 1 1 ~
            tail = os.read(fd, 2)
            if tail == b"1~":
                return "a11y"
            return "back"
        return {b"[A": "up", b"[B": "down", b"[5": "prev", b"[6": "next"}.get(seq, "back")
    return {b"\r": "enter", b"\n": "enter", b"q": "quit", b"n": "new"}.get(c, None)


def _con_pick(items, label, render):
    """Un menu minimal, avec les mêmes flèches. Sert au choix de l'org et de
    l'app — c'est-à-dire la seule navigation que le client s'autorise à
    connaître, parce qu'elle précède toute notion d'app."""
    i = 0
    fd = sys.stdin.fileno()
    old = termios.tcgetattr(fd)
    try:
        tty.setraw(fd)
        while True:
            # Largeur réelle : sur un Minitel (40 col) une ligne figée à 50 débordait et
            # partait à la ligne. On coupe au gabarit, et la barre de sélection couvre la
            # rangée entière (padding sur la ligne active seulement).
            try:
                w = os.get_terminal_size().columns
            except OSError:
                w = 80
            inner = max(4, w - 3)
            sys.stdout.write("\x1b[2J\x1b[H\x1b[1;36m" + label[:w] + "\x1b[0m\r\n\r\n")
            for k, it in enumerate(items):
                sel = k == i
                line = render(it)[:inner]
                if sel:
                    line = line.ljust(inner)
                mark = "\x1b[7m" if sel else ""
                sys.stdout.write(f"  {mark}{line}\x1b[0m\r\n")
            sys.stdout.write("\r\n\x1b[2m↑ ↓ choisir · ↵ valider · Échap retour · q quitter\x1b[0m\r\n")
            sys.stdout.flush()
            k = _con_read_key(fd)
            if k == "up":
                i = (i - 1) % len(items)
            elif k == "down":
                i = (i + 1) % len(items)
            elif k == "enter":
                return items[i]
            elif k == "quit":
                return "quit"       # sortie franche, distincte du Retour
            elif k == "back":
                return None         # Retour : l'appelant remonte d'un cran
    finally:
        termios.tcsetattr(fd, termios.TCSADRAIN, old)


def _con_pick_frame_accessible(app_url, token, role, lang, **extra):
    """Un écran de CHOIX en liste numérotée : le dashboard (toutes les apps de toutes
    les orgs), ou le choix d'org d'une app fournie par plusieurs. Le texte a11y donne
    les numéros à l'humain ; la trame JSON dit où chaque numéro mène. Le client ne fait
    jamais la correspondance de tête — il la lit. Renvoie l'action choisie (un dict, avec
    `to` et parfois `org`), ou None."""
    q = dict(role=role, lang=lang, a11y=1, **extra)
    url = f"{app_url}/api/console/frame/?" + urllib.parse.urlencode(q)
    try:
        txt = _con_req(url, headers={"Authorization": f"Bearer {token}"}, raw=True).decode()
    except urllib.error.HTTPError as e:
        print(json.loads(e.read() or b"{}").get("error", f"Erreur HTTP {e.code}"))
        return None
    print()
    print(txt)

    # Il faut aussi la trame JSON : le texte donne les numéros à un humain, mais c'est
    # la trame qui dit à quelle APPLI (et dans quelle ORG) chaque numéro mène. Le
    # client n'invente jamais cette correspondance — il la lit.
    f = _con_frame(app_url, token, role=role, lang=lang, **extra)
    foc = f.get("focusables", [])
    try:
        n = input("> ").strip()
    except (EOFError, KeyboardInterrupt):
        return None
    if not n.isdigit() or not (1 <= int(n) <= len(foc)):
        return None
    return foc[int(n) - 1].get("action") or {}


def _con_home_accessible(app_url, token, role, lang):
    """Le DASHBOARD en liste numérotée : toutes les apps de toutes les orgs, comme le
    web. Renvoie (app_id, org_oid) de l'app choisie — l'org voyage dans l'action de
    la tuile (nulle si plusieurs orgs la fournissent : on la demande alors après le
    clic). Plus d'écran « choisir l'organisation » avant même de voir les apps."""
    act = _con_pick_frame_accessible(app_url, token, role, lang) or {}
    return (act.get("to"), act.get("org"))


def _con_resolve_org(app_url, token, app):
    """L'org d'une app fournie par UNE SEULE org : sa tuile du dashboard la porte. On
    relit le dashboard (le moteur), sans interroger aucun endpoint que le client aurait
    à connaître — on cherche la tuile de `app` et on prend l'org de son action. Les
    apps multi-org n'ont pas d'org sur leur tuile : celles-là passent par le choix (voir
    _con_choose_org). Renvoie l'oid, ou None."""
    try:
        f = _con_frame(app_url, token, role="collaborator")
    except Exception:
        return None
    for foc in f.get("focusables", []):
        act = foc.get("action") or {}
        if act.get("to") == app and act.get("org"):
            return act.get("org")
    return None


def _con_choose_org(a, token, app, cols, rows, center=False):
    """L'org de l'app, résolue APRÈS le clic — comme le web, et seulement si besoin.

    On demande au moteur la trame de l'app SANS org, et il décide :
      • une seule org la fournit → il l'adopte en silence et renvoie le SOMMAIRE (des
        `slots`). Rien à demander ; on lit l'oid sur la tuile du dashboard (mono-org).
      • plusieurs → il renvoie l'ÉCRAN DE CHOIX numéroté. On laisse l'utilisateur
        choisir ; l'org voyage dans l'action de la ligne, jamais inventée par le client.

    Renvoie l'oid choisi, ou None (annulé, ou app introuvable)."""
    probe = _con_frame(a.app_url, token, app=app, role=a.role, lang=a.lang)
    if "slots" in probe:
        # Une seule org : le moteur l'a déjà adoptée. Son oid est sur la tuile.
        return _con_resolve_org(a.app_url, token, app)
    if "error" in probe and "focusables" not in probe:
        print(probe["error"])
        return None
    # Plusieurs orgs → le moteur pose la question. On la laisse à l'utilisateur, dans
    # le mode où il est : la même boucle de trame en visuel, la liste numérotée en a11y.
    if a.accessible:
        act = _con_pick_frame_accessible(a.app_url, token, a.role, a.lang, app=app)
    else:
        act = _con_run(a.app_url, token, app, None, a.role, None, cols, rows, a.lang, center)
    # `_con_run` peut renvoyer "back"/"quit" (des chaînes) si l'utilisateur ressort du
    # choix d'org : ce n'est pas une org, c'est une annulation.
    return act.get("org") if isinstance(act, dict) else None


# ── La boucle ───────────────────────────────────────────────────────────────

def _con_welcome(a):
    """L'écran d'accueil, affiché une fois après le login.

    C'est ici que l'accessibilité devient une OPTION qu'on active, et pas seulement un
    argument de lancement : on appuie sur F1 pour le mode vocal (liste numérotée,
    sémantique allégée), n'importe quelle autre touche pour le mode visuel. « Au début,
    au login » — exactement là où on l'attend.

    Renvoie True si F1 a été pressé. Ne fait rien si --accessible est déjà passé :
    proposer d'activer ce qui l'est déjà n'a pas de sens."""
    if a.accessible:
        return True
    fd = sys.stdin.fileno()
    try:
        old = termios.tcgetattr(fd)
    except (termios.error, ValueError):
        return False                     # pas un vrai terminal (pipe, cron) : mode visuel
    try:
        tty.setraw(fd)
        sys.stdout.write(
            "\x1b[2J\x1b[H"
            "\x1b[36;1m      R E D S T A R S\x1b[0m\r\n\r\n"
            "  \x1b[2m" + "\u2500" * 34 + "\x1b[0m\r\n\r\n"
            "  Bienvenue.\r\n\r\n"
            "  \x1b[33mF1\x1b[0m  mode accessibilite (voix,\r\n"
            "      liste numerotee)\r\n\r\n"
            "  \x1b[2mune autre touche : mode visuel\x1b[0m\r\n")
        sys.stdout.flush()
        return _con_read_key(fd) == "a11y"
    finally:
        termios.tcsetattr(fd, termios.TCSADRAIN, old)


# Les libellés du login, fidèles au web (frontend auth/login/page.tsx) : la même
# marque, les mêmes mots. Fr par défaut ; en si le terminal est lancé --lang en.
_LOGIN_L10N = {
    "fr": {"sub": "Plateforme de gestion multi-services",
           "user": "Nom d'utilisateur", "pw": "Mot de passe", "ph": "votre_pseudo",
           "envoi": "ENVOI", "submit": "se connecter",
           "noacc": "Pas de compte ? Inscription sur le web.",
           "hint": "↵ valider  ·  ↑↓ champ  ·  Echap annuler",
           "f1": "F1 · accessibilité (voix, liste numérotée)",
           "cont": "CONTINUER", "contsub": "session déjà ouverte",
           "hint2": "↵ continuer  ·  Echap quitter"},
    "en": {"sub": "Multi-service management platform",
           "user": "Username", "pw": "Password", "ph": "your_username",
           "envoi": "ENTER", "submit": "sign in",
           "noacc": "No account? Register on the web.",
           "hint": "↵ submit  ·  ↑↓ field  ·  Esc cancel",
           "f1": "F1 · accessibility (speech, numbered list)",
           "cont": "CONTINUE", "contsub": "session already open",
           "hint2": "↵ continue  ·  Esc quit"},
}

_ANSI_RE = re.compile(r"\x1b\[[0-9;?]*[A-Za-z]")


def _con_login_read(fd):
    """Une frappe → un caractère complet (UTF-8) ou une touche de contrôle.

    getpass lit une LIGNE ; un formulaire a besoin de la frappe caractère par
    caractère, pour dessiner les champs et masquer le mot de passe en direct.
    On assemble les séquences UTF-8 multi-octets (un mot de passe peut en avoir)
    d'après l'octet de tête, sinon un é deviendrait deux points au lieu d'un."""
    b = os.read(fd, 1)
    if not b:
        return ("key", "esc")
    o = b[0]
    if o == 0x1b:                              # ESC seul, ou séquence CSI/SS3
        seq = os.read(fd, 2)
        if seq == b"OP":                       # F1 VT100/SS3
            return ("key", "a11y")
        if seq == b"[1":                       # F1 xterm possible : ESC [ 1 1 ~
            return ("key", "a11y") if os.read(fd, 2) == b"1~" else ("key", "esc")
        return ("key", {b"[A": "up", b"[B": "down"}.get(seq, "esc"))
    if o in (0x0d, 0x0a):
        return ("key", "enter")
    if o == 0x09:
        return ("key", "tab")
    if o in (0x7f, 0x08):
        return ("key", "back")
    if o in (0x03, 0x04):                      # Ctrl-C / Ctrl-D
        return ("key", "quit")
    if o < 0x20 or 0x80 <= o < 0xc0:           # autre contrôle, ou continuation orpheline
        return ("key", "skip")
    if o < 0x80:
        return ("char", chr(o))
    n = 2 if o < 0xe0 else 3 if o < 0xf0 else 4   # tête UTF-8 → longueur totale
    try:
        return ("char", (b + os.read(fd, n - 1)).decode("utf-8"))
    except UnicodeDecodeError:
        return ("key", "skip")


def _con_login_screen(a, logged_in=False):
    """L'écran de marque RedStars — la réplique Vidéotex du login web, montré À CHAQUE
    lancement, y compris quand la session est déjà ouverte.

    Deux modes, même marque :
      • login (pas de session) : deux champs, le mot de passe masqué au fur et à mesure.
        Renvoie (utilisateur, mot_de_passe).
      • `logged_in` (session déjà là) : PAS de champs, juste un bouton « Continuer ».
        Renvoie "continue" (on entre) ou "quit" (Echap).

    Dans les deux cas la mention F1 (accessibilité) est ICI — il n'y a plus d'écran
    d'accueil séparé. F1 bascule le mode accessible et poursuit.

    Renvoie None (mode login) quand ce n'est pas un vrai terminal (pipe, cron) ou en mode
    accessible : l'appelant retombe alors sur la saisie ligne à ligne, sûre pour un
    lecteur d'écran. En `logged_in`, ces mêmes cas renvoient "continue" (rien à saisir)."""
    if a.accessible:
        return "continue" if logged_in else None   # lecteur d'écran : pas d'écran spatial
    fd = sys.stdin.fileno()
    try:
        old = termios.tcgetattr(fd)
    except (termios.error, ValueError):
        return "continue" if logged_in else None    # pas un tty
    tr = _LOGIN_L10N.get(a.lang, _LOGIN_L10N["fr"])
    try:
        tcols = os.get_terminal_size().columns
    except OSError:
        tcols = 40
    CW = 40 if a.minitel else max(40, min(tcols, 80))
    IW = 32                                     # largeur intérieure du cadre
    fw = IW - 4                                 # largeur visible d'un champ (entre crochets)

    C_BRAND, C_STAR, C_DIM = "\x1b[36;1m", "\x1b[33m", "\x1b[2m"
    C_FRAME, C_ACT, C_OK, R = "\x1b[34m", "\x1b[36m", "\x1b[32;1m", "\x1b[0m"

    fields = ["", ""]                           # [utilisateur, mot de passe]
    cur = 0

    def center(s):
        pad = max(0, (CW - len(_ANSI_RE.sub("", s))) // 2)
        return " " * pad + s

    def frame_row(plain, color=""):
        body = (" " + plain).ljust(IW)
        return center(f"{C_FRAME}│{R}{color}{body}{R}{C_FRAME}│{R}")

    def frame_field(idx, masked, placeholder=""):
        raw = fields[idx]
        if not raw and idx != cur and placeholder:
            return frame_row("[" + placeholder.ljust(fw)[:fw] + "]", C_DIM)
        shown = ("•" * len(raw)) if masked else raw
        if idx == cur:
            shown += "█"                   # curseur : un bloc plein sur le champ actif
        if len(shown) > fw:                      # champ plein : on montre la fin
            shown = shown[len(shown) - fw:]
        return frame_row("[" + shown.ljust(fw) + "]", C_ACT if idx == cur else C_DIM)

    def screen():
        head = [
            "",
            center(f"{C_STAR}★{R}  {C_BRAND}R E D S T A R S{R}  {C_STAR}★{R}"),
            center(f"{C_DIM}{tr['sub']}{R}"),
            "",
        ]
        if logged_in:
            # Déjà connecté : pas de champs, un bouton « Continuer » en vidéo inverse.
            body = [
                "",
                center(f"{C_OK}\x1b[7m  ▶  {tr['cont']}  {R}"),
                "",
                center(f"{C_DIM}{tr['contsub']}{R}"),
                "",
                center(f"{C_OK}{tr['envoi']}{R} {C_DIM}{tr['cont'].lower()}{R}"),
            ]
        else:
            body = [
                center(f"{C_FRAME}┌{'─' * IW}┐{R}"),
                frame_row(tr["user"], C_ACT if cur == 0 else C_DIM),
                frame_field(0, False, tr["ph"]),
                frame_row(""),
                frame_row(tr["pw"], C_ACT if cur == 1 else C_DIM),
                frame_field(1, True),
                center(f"{C_FRAME}└{'─' * IW}┘{R}"),
                "",
                center(f"{C_OK}{tr['envoi']}{R} {C_DIM}{tr['submit']}{R}"),
                center(f"{C_DIM}{tr['noacc']}{R}"),
            ]
        # La mention F1 vit ICI, sur l'écran de marque — plus d'écran d'accueil séparé.
        foot = [
            "",
            center(f"{C_STAR}{tr['f1']}{R}"),
            center(f"{C_DIM}{tr['hint2'] if logged_in else tr['hint']}{R}"),
        ]
        return head + body + foot

    try:
        tty.setraw(fd)
        while True:
            sys.stdout.write("\x1b[?25l\x1b[2J\x1b[H" + "\r\n".join(screen()) + "\r\n")
            sys.stdout.flush()
            kind, v = _con_login_read(fd)
            if v == "a11y":                         # F1 : bascule accessibilité, ici même
                a.accessible = True
                return "continue" if logged_in else None
            if logged_in:
                # Un seul choix : continuer (↵ / une touche) ou quitter (Echap).
                if v == "enter" or kind == "char":
                    return "continue"
                if v in ("esc", "quit"):
                    return "quit"
                continue                            # autre contrôle : on reste
            if kind == "char":
                fields[cur] += v
            elif v == "back":
                fields[cur] = fields[cur][:-1]
            elif v in ("tab", "down"):
                cur = 1 - cur
            elif v == "up":
                cur = 0
            elif v == "enter":
                if cur == 0:
                    cur = 1                      # ENVOI depuis l'identifiant → passe au mot de passe
                elif fields[0] and fields[1]:
                    return (fields[0], fields[1])
                else:
                    cur = 0 if not fields[0] else 1
            elif v in ("esc", "quit"):
                return None
            # "skip" : frappe ignorée (contrôle non géré)
    finally:
        termios.tcsetattr(fd, termios.TCSADRAIN, old)
        sys.stdout.write("\x1b[?25h\x1b[2J\x1b[H")   # curseur rendu, écran nettoyé
        sys.stdout.flush()


def _con_run(app_url, token, app, org, role, slot, cols, rows, lang, center=False,
             modal=None, item=None, src=None):
    # On suit le NUMÉRO du focusable, pas sa ligne.
    #
    # Une ligne pouvait porter une seule chose à sélectionner — jusqu'à l'accueil, où
    # trois tuiles d'application partagent une rangée. Repérer le curseur par sa ligne
    # devenait alors ambigu : les flèches atterrissaient sur la première tuile de la
    # rangée, et Entrée ouvrait une autre appli que celle qu'on croyait viser.
    #
    # Le numéro est le même que celui qu'on tape en mode accessible. Une seule notion
    # de « quoi est sélectionné », partagée par les deux modes.
    sel = 1
    fd = sys.stdin.fileno()
    old = termios.tcgetattr(fd)
    try:
        tty.setraw(fd)
        while True:
            # Le serveur pagine tout seul : on lui donne le curseur, il place la
            # fenêtre. Le client n'a pas d'état de défilement à tenir — et donc pas
            # d'occasion de le désynchroniser.
            f = _con_frame(app_url, token, app=app, org=org, role=role, slot=slot,
                      modal=modal, item=item, src=src,
                      sel=sel, cols=cols, rows=rows, lang=lang)
            if "error" in f:
                sys.stdout.write("\x1b[2J\x1b[H\x1b[31m" + f["error"].replace("\n", "\r\n") + "\x1b[0m\r\n\r\n")
                sys.stdout.write("\x1b[2mUne touche pour revenir…\x1b[0m")
                sys.stdout.flush()
                os.read(fd, 1)
                return "back"

            # Le serveur a déjà tout composé. On imprime — centré en mode Minitel, où le
            # cadre 40×24 flotte dans une console plus large que lui.
            sys.stdout.write(_con_center(f["ansi"], cols, rows) if center
                             else f["ansi"].replace("\n", "\r\n"))
            # Les images du réseau de rendu (avatars, vignettes) sont posées PAR-DESSUS la
            # trame : décodées et rendues en local (le décodeur nn: n'existe pas côté serveur).
            if f.get("images"):
                t, l = _con_offset(cols, rows, center)
                _con_blit_images(f["images"], t, l)
            sys.stdout.flush()

            k = _con_read_key(fd)

            # Toutes les lignes ne sont pas sélectionnables — un titre, une ligne vide,
            # un en-tête de colonnes n'ont rien à choisir. On se déplace donc de
            # focusable en focusable, et c'est la TRAME qui dit lesquels, pas nous.
            foc = f.get("focusables", [])
            here = next((i for i, x in enumerate(foc) if x.get("n") == sel), 0)
            step = max(1, rows - 5)

            if k == "down" and foc:
                sel = foc[min(here + 1, len(foc) - 1)]["n"]
            elif k == "up" and foc:
                sel = foc[max(here - 1, 0)]["n"]
            elif k == "next" and foc:
                sel = foc[min(here + step, len(foc) - 1)]["n"]
            elif k == "prev" and foc:
                sel = foc[max(here - step, 0)]["n"]
            elif k == "enter":
                # Ce que fait Entrée est DIT PAR LA TRAME, pas décidé ici. Le client
                # ne sait pas ce qu'est une application ; il sait suivre une route.
                if 0 <= here < len(foc):
                    act = foc[here].get("action") or {}
                    kind = act.get("kind")
                    if kind == "route":
                        return act
                    if kind == "open-modal":
                        # Descendre d'un cran : le DÉTAIL plein écran (fiche membre, détail
                        # de ligne). La modale est une trame comme une autre — le moteur rend
                        # le composant de l'app. Retour y revient à la liste ; Quitter sort.
                        sub = _con_run(app_url, token, app, org, role, None, cols, rows, lang,
                                       center, modal=act.get("modal"), item=act.get("itemId"),
                                       src=slot)
                        if sub == "quit":
                            return "quit"
                        # "back"/None → on reste dans la liste (la boucle la redessine)
                    # kind == "none"/inconnu : rien à ouvrir, on ignore l'Entrée
            elif k == "back":
                # Retour = remonter d'UN cran (slot → sommaire), pas quitter. C'est
                # l'appelant qui décide où mène ce cran ; ici on dit seulement « en haut ».
                return "back"
            elif k == "quit":
                return "quit"
    finally:
        termios.tcsetattr(fd, termios.TCSADRAIN, old)
        sys.stdout.write("\x1b[2J\x1b[H")
        sys.stdout.flush()


def _con_modal_accessible(app_url, token, app, org, role, modal, item, src, lang):
    """Le DÉTAIL (fiche membre, détail de ligne) en texte pur, pour lecteur d'écran.
    Le moteur rend le composant de l'app ; on n'écrit aucune vue de plus."""
    q = {k: v for k, v in dict(app=app, org=org, role=role, modal=modal, item=item,
                               src=src, lang=lang, a11y=1).items() if v is not None}
    url = f"{app_url}/api/console/frame/?" + urllib.parse.urlencode(q)
    try:
        txt = _con_req(url, headers={"Authorization": f"Bearer {token}"}, raw=True).decode()
    except urllib.error.HTTPError as e:
        print(json.loads(e.read() or b"{}").get("error", f"Erreur HTTP {e.code}"))
        return
    print()
    print(txt)
    try:
        input("\n(Entrée pour revenir) ")
    except (EOFError, KeyboardInterrupt):
        pass


def _con_run_accessible(app_url, token, app, org, role, slot, lang):
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
            txt = _con_req(url, headers={"Authorization": f"Bearer {token}"}, raw=True).decode()
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
            f = _con_frame(app_url, token, app=app, org=org, role=role, slot=slot, lang=lang)
            hit = next((x for x in f.get("focusables", []) if x["n"] == int(answer)), None)
            if not hit:
                print(f"Il n'y a pas d'élément numéro {answer}.")
                continue
            act = hit.get("action") or {}
            if act.get("kind") == "open-modal":
                # Le détail, plein texte — on descend d'un cran, comme le web.
                _con_modal_accessible(app_url, token, app, org, role, act.get("modal"),
                                      act.get("itemId"), slot, lang)
                continue
            print(f"\n{hit['speech']}")
            print("(pas d'action disponible ici)")
            continue
        print("Tapez un numéro, ou 0 pour revenir.")


# ── Handoff console : le navigateur a le jeton, nous avons le terminal ───────
#
# Le raccourci évident était `redstars-helper://console?token=eyJ…` : le
# navigateur ouvre l'URI, xdg-open la passe au helper. Sauf que xdg-open la
# passe en ARGV — et argv est lisible par TOUS les utilisateurs de la machine
# (`ps aux`), en plus d'atterrir dans la base « récemment utilisé » du bureau et,
# selon les systèmes, dans journald. Un jeton de session dans une URI, c'est un
# jeton de session offert à quiconque a un shell sur la même machine.
#
# On passe donc par la socket loopback qui existe DÉJÀ et à laquelle la page de
# login parle déjà (l'activation WebGPU). Le jeton fait navigateur → 127.0.0.1 →
# variable d'environnement → processus fils. Il ne touche jamais une URL, jamais
# une ligne de commande, jamais un log.

# `dbus` marque les terminaux qui ne lancent PAS la commande eux-mêmes : ils la
# font exécuter par un serveur déjà en cours (gnome-terminal-server, ptyxis-agent,
# kgx). Le shell fils hérite alors de l'environnement du SERVEUR, pas du nôtre —
# et Popen renvoie 0 quand même. C'est pour eux que le jeton ne peut pas voyager
# par une variable d'environnement. Voir _con_token_file().
_CON_TERMINALS = [
    # nom,             préfixe d'args,        activé par D-Bus ?
    ("foot",           ["-e"],                False),
    ("alacritty",      ["-e"],                False),
    ("ghostty",        ["-e"],                False),
    ("kitty",          [],                    False),
    ("wezterm",        ["start", "--"],       False),
    ("ptyxis",         ["--"],                True),   # défaut GNOME/Fedora 41+
    ("kgx",            ["-e"],                True),   # GNOME Console
    ("gnome-terminal", ["--"],                True),
    ("konsole",        ["-e"],                False),
    ("xfce4-terminal", ["-x"],                False),
    ("tilix",          ["-e"],                False),
    ("terminator",     ["-x"],                False),
    ("mate-terminal",  ["--"],                True),
    ("lxterminal",     ["-e"],                False),
    ("deepin-terminal",["-e"],                False),
    ("urxvt",          ["-e"],                False),
    ("st",             ["-e"],                False),
    ("xterm",          ["-e"],                False),
    ("x-terminal-emulator", ["-e"],           False),  # l'alternative Debian
]

# Un helper lancé par le BUREAU n'a pas le PATH de ton shell de connexion. On
# cherche donc aussi dans les répertoires habituels : « aucun terminal trouvé »
# alors que /usr/bin/ptyxis existe, c'est une réponse fausse, pas une absence.
_CON_BINDIRS = [
    "/usr/bin", "/usr/local/bin", "/bin", "/opt/bin",
    os.path.expanduser("~/.local/bin"),
    "/var/lib/flatpak/exports/bin",
    os.path.expanduser("~/.local/share/flatpak/exports/bin"),
    "/snap/bin",
]


def _con_find(term):
    """Trouve un binaire même quand PATH est amputé (lancement par le bureau)."""
    import shutil
    p = shutil.which(term)
    if p:
        return p
    for d in _CON_BINDIRS:
        c = os.path.join(d, term)
        if os.path.isfile(c) and os.access(c, os.X_OK):
            return c
    return None


def _con_token_file(token):
    """Dépose le jeton dans un fichier que seul l'utilisateur peut lire.

    Pourquoi pas une variable d'environnement, comme prévu au départ : les
    terminaux GNOME (gnome-terminal, kgx, ptyxis) sont activés par D-Bus. Ils ne
    lancent pas notre commande — ils la font exécuter par un serveur déjà vivant,
    dont l'environnement n'est pas le nôtre. Le jeton n'arriverait jamais, la
    console redemanderait le mot de passe, et Popen aurait quand même renvoyé 0 :
    un succès qui n'en est pas un.

    Le CHEMIN voyage donc dans argv — un chemin n'est pas un secret — et le jeton
    reste dans un fichier 0600, sous $XDG_RUNTIME_DIR (tmpfs, 0700, propre à
    l'utilisateur). Le client le lit et le supprime AUSSITÔT : la fenêtre
    d'exposition se compte en millisecondes, et l'exposition elle-même est celle
    de /proc/<pid>/environ — le propriétaire et root — pas celle d'argv, que tout
    le monde peut lire.
    """
    import secrets
    import tempfile
    d = os.environ.get("XDG_RUNTIME_DIR") or tempfile.gettempdir()
    path = os.path.join(d, f"redstars-console-{secrets.token_hex(8)}.tok")
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "w") as f:
        f.write(token)
    return path


def _con_spawn_terminal(token, opts):
    """Ouvre un terminal sur le client console, déjà authentifié."""
    import subprocess

    script = os.path.abspath(__file__)
    tokfile = _con_token_file(token)
    argv = [sys.executable or "python3", script, "console", "--token-file", tokfile]
    for k in ("app", "role", "lang", "app-url", "api-url"):
        v = opts.get(k.replace("-", "_"))
        if v:
            argv += [f"--{k}", str(v)]
    if opts.get("minitel"):
        argv.append("--minitel")
    if opts.get("accessible"):
        argv.append("--accessible")

    # shlex.quote sur CHAQUE argument : ces valeurs viennent d'une requête HTTP,
    # et elles finissent dans un `sh -lc`. Sans ça, un `app` valant `x; rm -rf ~`
    # serait exécuté. L'origine est déjà filtrée, mais une défense qui repose sur
    # une seule barrière n'est pas une défense.
    import shlex
    cmd = " ".join(shlex.quote(a) for a in argv)
    inner = f"{cmd}; echo; read -p 'Entrée pour fermer…' _"

    # On passe AUSSI le jeton par l'environnement : pour les terminaux qui ne sont
    # pas activés par D-Bus, il arrive directement et le fichier n'est même pas lu.
    # Ceinture et bretelles, dans cet ordre : le fichier est la ceinture.
    env = dict(os.environ)
    env["REDSTARS_TOKEN"] = token

    terms = list(_CON_TERMINALS)
    if os.environ.get("TERMINAL"):
        terms.insert(0, (os.environ["TERMINAL"], ["-e"], False))

    for term, pre, _dbus in terms:
        path = _con_find(term)
        if not path:
            continue
        try:
            subprocess.Popen([path, *pre, "sh", "-lc", inner], env=env,
                             start_new_session=True)
            return os.path.basename(path)
        except Exception:
            continue

    # Personne n'ouvrira ce fichier : on ne laisse pas traîner un jeton de session
    # dans le runtime dir parce qu'aucun terminal n'a été trouvé.
    try:
        os.unlink(tokfile)
    except OSError:
        pass
    return None


def _con_bigfont_setup():
    """Sur une VRAIE console Linux (TERM=linux), agrandit la police pour un rendu
    proche du Minitel — de gros caractères qui remplissent l'écran. Sauve la police
    courante et renvoie une fonction de restauration (à appeler en sortie), ou None si
    on n'est pas sur une console texte, si `setfont` manque, ou si aucune grosse police
    n'a pris.

    Pourquoi setfont, et pas une séquence d'échappement : la console Linux n'a AUCUNE
    commande in-band pour la police. Elle ignore aussi le double-hauteur VT100
    (`ESC # 3/4`) et le resize XTWINOPS (`ESC [ 8 t`) — donc les astuces des émulateurs
    ne servent à rien ici. La taille des glyphes s'y change avec `setfont`, point.

    On agrandit surtout la HAUTEUR (police 32 px de haut) : le nombre de colonnes vaut
    largeur_écran ÷ largeur_police, or les polices console plafonnent à ~16 px de large
    — viser 40 colonnes pile demanderait en plus une résolution basse (root/boot). Une
    police 16×32 donne déjà ~24 lignes plein écran, la bonne proportion, et le cadre est
    centré dans les colonnes restantes (voir _con_center)."""
    import shutil
    import subprocess
    import tempfile
    if os.environ.get("TERM") != "linux" or not sys.stdout.isatty():
        return None
    if not shutil.which("setfont"):
        return None
    old = os.path.join(tempfile.gettempdir(), f"redstars-oldfont-{os.getpid()}.psf")
    # De la plus grande à la plus petite, en couvrant les noms usuels selon la distrib
    # (kbd, console-setup, terminus). `-o old` sauve la police courante AVANT de charger
    # la nouvelle : dès qu'un chargement réussit, `old` contient bien la police d'origine
    # (les essais ratés n'ont rien changé). On garde la première que setfont accepte.
    for font in ("ter-v32b", "latarcyrheb-sun32", "Uni3-TerminusBold32x16",
                 "Lat15-Terminus32x16", "Lat2-Terminus32x16", "Uni2-Terminus32x16",
                 "sun12x22", "ter-v24b", "ter-v22b"):
        try:
            r = subprocess.run(["setfont", "-o", old, font],
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        except Exception:
            return None
        if r.returncode == 0:
            def restore(_old=old):
                try:
                    subprocess.run(["setfont", _old],
                                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                except Exception:
                    pass
                try:
                    os.unlink(_old)
                except OSError:
                    pass
            return restore
    return None


def _con_center(ansi, cols, rows):
    """Centre le cadre du serveur dans le terminal réel — pour le mode Minitel, où le
    contenu fait 40×24 mais la console (surtout après un gros setfont) reste plus large.

    La trame ANSI est orientée LIGNES : un effacement + « home » en tête, puis des lignes
    jointes par CRLF, sans positionnement absolu par ligne. On retire l'effacement/home,
    on découpe, et on ré-émet chaque ligne à une position absolue (row, col) pour poser
    le bloc 40×24 au centre. La taille réelle est relue À CHAQUE trame : setfont a pu
    changer la géométrie de la console entre-temps."""
    try:
        sz = os.get_terminal_size()
        rc, rr = sz.columns, sz.lines
    except OSError:
        rc, rr = cols, rows
    left = max(0, (rc - cols) // 2)
    top = max(0, (rr - rows) // 2)
    body = ansi.replace("\x1b[2J", "").replace("\x1b[H", "")
    out = ["\x1b[2J"]
    for i, ln in enumerate(body.split("\n")):
        out.append(f"\x1b[{top + i + 1};{left + 1}H" + ln.rstrip("\r"))
    return "".join(out)


def _con_offset(cols, rows, center):
    """Coin haut-gauche du cadre dans le terminal réel : (0,0) en mode normal (la trame
    commence à « home »), centré en mode Minitel — la même arithmétique que _con_center."""
    if not center:
        return 0, 0
    try:
        sz = os.get_terminal_size()
        rc, rr = sz.columns, sz.lines
    except OSError:
        rc, rr = cols, rows
    return max(0, (rr - rows) // 2), max(0, (rc - cols) // 2)


# img_render (décodeur nn: + rendu quadrant) vit CÔTÉ BUNDLE TAURI, pas dans helper.py :
# numpy/onnxruntime + le modèle 3,7 Mo y sont, pour que helper.py (signé, auto-updaté seul
# dans le cache OS) reste léger. On le cherche dans le bundle Tauri —
# helper.py auto-updaté n'a PAS img_render.py à côté de lui, il faut aller le chercher.
_IMG = {'mod': None, 'tried': False}

def _img_dirs():
    import glob as _glob
    cands = [DEMO_DIR]
    env = os.environ.get('REDSTARS_HELPER_BUNDLED_DIR')
    if env:
        cands.append(Path(env))
    for pat in (
        '/usr/lib/*/bundled/img_render.py', '/usr/lib/*/resources/bundled/img_render.py',
        '/usr/share/*/bundled/img_render.py', '/opt/*/bundled/img_render.py',
        '/tmp/.mount_*/usr/lib/*/bundled/img_render.py',
        '/Applications/*.app/Contents/Resources/bundled/img_render.py',
        str(Path.home() / 'Applications' / '*.app' / 'Contents' / 'Resources' / 'bundled' / 'img_render.py'),
    ):
        for hit in _glob.glob(pat):
            cands.append(Path(hit).parent)
    return cands

def _img_mod():
    """Charge paresseusement img_render depuis le bundle Tauri. None si absent (numpy/onnx
    ou modèle manquants) → le repli texte du serveur reste."""
    if _IMG['tried']:
        return _IMG['mod']
    _IMG['tried'] = True
    try:
        import sys as _sys
        for d in _img_dirs():
            if (d / 'img_render.py').is_file():
                if str(d) not in _sys.path:
                    _sys.path.insert(0, str(d))
                break
        import img_render
        _IMG['mod'] = img_render
    except Exception:
        _IMG['mod'] = None
    return _IMG['mod']

def _con_img_capable():
    m = _img_mod()
    try:
        return bool(m and m.capable())
    except Exception:
        return False


def _con_blit_images(images, top, left):
    """Poser les images du RÉSEAU DE RENDU (avatars, vignettes) par-dessus la trame.

    Le serveur ne peut pas les rendre : le décodeur `nn:` ne tourne pas côté serveur. Il
    marque donc juste OÙ et QUEL hash — `images: [{hash, row, col, w, h}]`, coordonnées en
    cellules dans le cadre. Ici, sur la machine de l'utilisateur, on décode le hash en local
    (img_render + modèle bundlé), on rend la tuile en texte quadrant — le MÊME moteur que le
    Web — et on la blitte à sa position. Sans décodeur (modèle/onnxruntime absents), on ne
    touche à rien : le repli texte que le serveur a déjà dessiné (initiales/glyphe) reste."""
    img = _img_mod()
    if img is None:
        return
    for im in images or []:
        h = im.get("hash")
        wc, hc = int(im.get("w", 0)), int(im.get("h", 0))
        row, col = int(im.get("row", 0)), int(im.get("col", 0))
        if not h or wc <= 0 or hc <= 0:
            continue
        try:
            lines = img.render_tile(h, wc, hc)
        except Exception:
            lines = None
        if not lines:
            continue
        for ri, ln in enumerate(lines):
            sys.stdout.write(f"\x1b[{top + row + ri + 1};{left + col + 1}H" + ln)


def run_console(argv):
    ap = argparse.ArgumentParser(prog="helper.py console", description="RedStars en console")
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
    ap.add_argument("--token-file", help=argparse.SUPPRESS)   # écrit par /helper/console
    ap.add_argument("--accessible", action="store_true",
                    help="liste numérotée, texte pur, sans curseur — pour lecteur d'écran")
    a = ap.parse_args(argv)

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

    # Négocier la profondeur de couleur une fois, ici : truecolor sur un émulateur qui
    # l'annonce (icônes au rendu Web), 256 couleurs partout ailleurs et en Minitel.
    global _CON_COLOR, _CON_IMG
    _CON_COLOR = _con_color_depth(a.minitel)
    # Capacité image : uniquement sur un terminal truecolor (une tuile d'avatar en
    # mosaïque n'a pas de sens en Videotex/256) ET si le décodeur bundlé est là.
    _CON_IMG = (not a.minitel) and _CON_COLOR == "truecolor" and _con_img_capable()

    # Mode Minitel : donner à la FENÊTRE la forme d'un Minitel, 40×24, et remplir
    # l'écran de gros caractères.
    #
    # Deux mondes, deux leviers :
    #   • Un ÉMULATEUR (xterm, kitty, gnome-terminal…) honore le resize DECSLPP/XTWINOPS
    #     `ESC [ 8 ; rows ; cols t` — la fenêtre prend la forme 40×24. Les autres
    #     l'ignorent (inoffensif). La police, elle, reste l'affaire de l'émulateur.
    #   • Une VRAIE console Linux ignore CE resize (et le double-hauteur VT100) : là, on
    #     agrandit la POLICE avec setfont, ce qu'un programme PEUT faire sur la console,
    #     puis on centre le cadre 40×24 dans les colonnes restantes.
    # On restaure taille et police d'origine à la sortie — y compris sur sys.exit — via
    # atexit, pour ne pas laisser la fenêtre rétrécie ni la console en 32 px.
    center = False
    if a.minitel and sys.stdout.isatty():
        sys.stdout.write("\x1b[8;24;40t")
        sys.stdout.flush()
        import atexit
        atexit.register(lambda oc=tc, orr=tr: (sys.stdout.write(f"\x1b[8;{orr};{oc}t"),
                                               sys.stdout.flush()))
        restore_font = _con_bigfont_setup()
        if restore_font:
            atexit.register(restore_font)
        center = True

    # Le navigateur a DÉJÀ authentifié l'utilisateur : il nous passe le jeton de
    # session, et on saute la saisie du mot de passe. Le jeton arrive par
    # l'ENVIRONNEMENT, jamais par argv — `ps aux` montre argv à tous les
    # utilisateurs de la machine, et un jeton de session dans argv, c'est un
    # jeton de session offert à quiconque a un shell sur la même machine.
    #
    # Pour la même raison il n'y a volontairement PAS de `--token` : une option
    # qu'on n'expose pas est une option que personne ne mettra dans un script.
    # Deux canaux, et le fichier est le fiable. Les terminaux GNOME (gnome-terminal,
    # kgx, ptyxis) sont activés par D-Bus : ils ne nous lancent pas, ils demandent à
    # un serveur déjà vivant de le faire, et ce serveur n'a jamais vu notre
    # environnement. Un jeton passé par REDSTARS_TOKEN n'y survit pas — et le
    # terminal s'ouvre quand même, et redemande le mot de passe.
    #
    # Le CHEMIN du fichier voyage dans argv, ce qui est sans danger : un chemin n'est
    # pas un secret. Le jeton, lui, est dans un fichier 0600 sous $XDG_RUNTIME_DIR
    # qu'on supprime AVANT de faire quoi que ce soit d'autre — pas après, pas plus
    # tard, pas dans un finally : le seul moment où l'on est sûr d'y penser.
    token = ""
    if a.token_file:
        try:
            with open(a.token_file, "r") as f:
                token = f.read().strip()
        except OSError:
            token = ""
        finally:
            try:
                os.unlink(a.token_file)
            except OSError:
                pass
    token = token or os.environ.pop("REDSTARS_TOKEN", "") or ""

    # Ordre : ce qu'on vient de recevoir, puis ce qu'on avait gardé, puis — en dernier
    # recours seulement — le mot de passe. Le taper dans un terminal (sans gestionnaire
    # de mots de passe, sans collage fiable, sans écho) est la friction qui fait qu'on
    # n'utilise pas la console. Elle ne doit se produire QU'UNE FOIS.
    if not token:
        token = _con_session_load(a.api_url) or ""

    if not token:
        # L'écran de login à la marque, dessiné ici parce qu'il précède l'auth (le
        # moteur exige déjà un jeton). On ne le montre que si RIEN n'est pré-fourni :
        # --user/--password sont là pour les scripts, et doivent court-circuiter la
        # saisie interactive, pas la décorer. F1 y bascule l'accessibilité.
        creds = None
        if not (a.user or a.password):
            creds = _con_login_screen(a)
        if creds:
            user, pw = creds
        else:
            user = a.user or input("Utilisateur : ")
            pw = a.password or getpass.getpass("Mot de passe : ")
        try:
            d = _con_req(f"{a.api_url}/api/v1/auth/login", {"username": user, "password": pw})
            token = d["access_token"]
            # On garde de quoi RENOUVELER, pas seulement de quoi entrer. Un jeton
            # d'accès seul vit quinze minutes : le stocker ne ferait que laisser
            # traîner un secret sans épargner la prochaine saisie.
            _con_session_save(token, d.get("refresh_token"), a.api_url)
        except urllib.error.HTTPError as e:
            sys.exit(f"Connexion refusée (HTTP {e.code}).")
    else:
        # Session DÉJÀ ouverte : on montre quand même l'écran de marque — logo, cadre —
        # mais avec un simple bouton « Continuer ». La mention F1 (accessibilité) est là
        # aussi : c'est le seul écran d'accueil, il n'y en a plus de séparé.
        if _con_login_screen(a, logged_in=True) == "quit":
            return

    # PAS de choix d'organisation : le DASHBOARD, comme le web — l'union des apps de
    # toutes les orgs de l'utilisateur. On ne choisit pas une org, on choisit une APP,
    # et l'org qui la fournit voyage avec elle (dans l'action de la tuile). L'ancien
    # écran « choisir l'organisation » n'existe plus : le dashboard montre déjà toutes
    # les apps de toutes les orgs, exactement comme la version Web.
    # Boucle de navigation : dashboard → app → sommaire → slot. « Retour » remonte d'UN
    # SEUL cran (slot → sommaire → dashboard → sortie) au lieu de tout quitter d'un coup.
    # C'est la remontée d'un Minitel (Sommaire/Retour) et du bouton Retour du web ; avant,
    # le flux était linéaire et le moindre Retour déroulait tout jusqu'à la sortie.
    pinned = bool(a.app)      # --app <id> : rien au-dessus de l'app, donc Retour = sortie
    while True:
        app = a.app
        org_oid = None
        if not app:
            if a.accessible:
                # Même document, autre sortie : le dashboard est déjà une liste numérotée
                # en accessible — mais ça renvoie du TEXTE, pas du JSON, donc un chemin à part.
                app, org_oid = _con_home_accessible(a.app_url, token, a.role, a.lang)
            else:
                act = _con_run(a.app_url, token, None, None, a.role, None, cols, rows, a.lang, center)
                # Au dashboard (le sommet), Retour comme Quitter = sortie : rien au-dessus.
                if not isinstance(act, dict):
                    return
                app = act.get("to")
                org_oid = act.get("org")
            if not app:
                return
        if not org_oid:
            # L'org, SI BESOIN — après le clic, comme le web. Une tuile mono-org l'a déjà
            # portée ; sinon (app multi-org, ou raccourci --app) le moteur tranche : il
            # l'adopte s'il n'y a qu'une org, ou pose la question s'il y en a plusieurs.
            org_oid = _con_choose_org(a, token, app, cols, rows, center)
            if not org_oid:
                # Choix d'org annulé (Retour) : on remonte au dashboard — ou on sort si
                # l'app était épinglée en ligne de commande (rien au-dessus).
                if pinned:
                    return
                continue
        org = {"oid": org_oid, "name": ""}

        # Le sommaire — et le recensement honnête : les slots `kind: null` sont du
        # React sur mesure, sans forme console. On les affiche barrés plutôt que de
        # les cacher : c'est une dette visible, pas un secret.
        s = _con_frame(a.app_url, token, app=app, org=org["oid"], role=a.role)
        if "error" in s:
            sys.exit(s["error"])

        # Le MENU de l'app — CELUI DU WEB : les mêmes libellés localisés, icônes, ordre et
        # groupe de pied (`views[role].menu`). Avant, la console listait les IDS de slots
        # bruts ; maintenant elle affiche le vrai menu, identique à la version Web, sans une
        # ligne de code par app. On retombe sur les slots si une vieille trame n'a pas `menu`.
        menu = s.get("menu") or [
            {"id": x["id"], "label": {"fr": x["id"], "en": x["id"]}, "kind": x.get("kind")}
            for x in s.get("slots", [])
        ]
        # Les entrées « align: bottom » (Réglages, etc.) filent en pied, comme sur le web.
        menu = [m for m in menu if m.get("align") != "bottom"] + \
               [m for m in menu if m.get("align") == "bottom"]

        def _menu_text(m):
            lab = m.get("label") or {}
            txt = lab.get(a.lang) or lab.get("fr") or m.get("id", "?")
            ico = (m.get("icon") or "").strip()
            return f"{ico} {txt}".strip() if ico else txt

        if a.accessible:
            # Le menu en liste numérotée. On ouvre N'IMPORTE quelle rubrique : le moteur
            # transcrit celles qui sont du React sur mesure, plus besoin de les cacher.
            print(f"\n{app} — {org['name']}.")
            print(f"{len(menu)} rubriques.\n")
            for i, m in enumerate(menu, 1):
                print(f"{i}. {_menu_text(m)}")
            print(f"\nTapez un numéro de 1 à {len(menu)} puis Entrée. 0 pour quitter.")
            try:
                n = input("> ").strip()
            except (EOFError, KeyboardInterrupt):
                return
            if not n.isdigit() or not (1 <= int(n) <= len(menu)):
                return
            _con_run_accessible(a.app_url, token, app, org["oid"], a.role, menu[int(n)-1]["id"], a.lang)
            return

        # Titre du menu : la trame peut en donner un joli (« Organisations » plutôt que
        # l'id brut) ; sinon on prend l'id de l'app. Le suffixe org ne s'ajoute que s'il
        # y a une org nommée (le pseudo-écran des orgs n'en a pas).
        menu_title = s.get("title") or app
        hdr = f"{menu_title} — {org['name']}" if org.get("name") else menu_title

        # Menu ↔ rubrique, en boucle : Retour depuis une rubrique revient au MENU ;
        # Retour depuis le menu casse cette boucle et remonte au dashboard.
        while True:
            choice = _con_pick(menu, hdr, _menu_text)
            if choice == "quit":
                return                   # Quitter (q) au menu = sortie franche
            if not choice:
                break                    # Retour (Échap) au menu → remonter au dashboard
            # On ouvre toute rubrique — le moteur rend le slot, ou transcrit le React.
            r = _con_run(a.app_url, token, app, org["oid"], a.role, choice["id"], cols, rows, a.lang, center)
            if r == "quit":
                return                   # Quitter (q) = sortie franche, depuis n'importe où
            # "back" / route consommée / None → on re-montre le menu (la boucle)

        # Sorti du sommaire par Retour : si l'app était épinglée (--app), rien au-dessus
        # → sortie ; sinon la boucle externe redessine le dashboard.
        if pinned:
            return


def _auto_update_loop(api_base, first_delay=8, period=6 * 3600):
    """Se mettre à jour TOUT SEUL. C'est censé être la définition d'un auto-update.

    `update_self()` existait déjà — mais rien ne l'appelait. Il n'était joignable que
    par `POST /helper/update`, c'est-à-dire par un BOUTON du tableau de bord. Donc une
    machine dont personne n'ouvrait ce panneau restait sur un helper.py vieux de
    plusieurs versions, indéfiniment, pendant que l'API annonçait la nouvelle. Un helper
    qui ne se met à jour que si on clique n'est pas un helper qui se met à jour.

    On ne redémarre PAS le processus après coup, et c'est délibéré : le démon tient des
    sockets (la balance, le serveur HTTP) et un execv() sous les pieds d'une requête en
    vol est un bug plus difficile que celui qu'on répare. Le fichier est écrit ; le
    client console, lui, est un processus NEUF à chaque lancement — il lit donc la
    nouvelle version immédiatement. Le démon prendra la sienne au prochain démarrage.
    """
    def run():
        time.sleep(first_delay)          # laisser le démon finir de s'ouvrir
        while True:
            try:
                r = update_self(api_base)
                if r.get('updated'):
                    print(f"[auto-update] helper.py {r.get('from_version')} → {r.get('version')}",
                          flush=True)
                elif r.get('error'):
                    # Hors ligne, API en vrac : ce n'est pas une erreur du helper, et
                    # ça ne doit ni le tuer ni remplir le journal.
                    pass
            except Exception as e:
                print(f"[auto-update] {type(e).__name__}: {e}", file=sys.stderr, flush=True)
            time.sleep(period)

    t = threading.Thread(target=run, daemon=True, name='auto-update')
    t.start()
    return t


def _cmd_status():
    """`redhelper status` — les DEUX versions (le shell « client », et ce
    helper.py), où vit ce fichier, et si le daemon local répond. Sans interface
    graphique : c'est le status qu'un terminal peut obtenir même quand le tray
    GUI refuse de démarrer (pas de display, lancé en root…). Le shell passe sa
    propre version via REDHELPER_SHELL_VERSION."""
    shell = os.environ.get('REDHELPER_SHELL_VERSION') or 'inconnue'
    print(f'client (shell)  : {shell}')
    print(f'helper.py       : {VERSION}')
    try:
        print(f'  fichier       : {os.path.realpath(__file__)}')
    except Exception:
        pass
    try:
        import urllib.request
        with urllib.request.urlopen(f'http://127.0.0.1:{PORT}/helper/status', timeout=2) as r:
            v = json.loads(r.read()).get('version', '?')
        print(f'  daemon :{PORT} : en marche (helper.py {v})')
    except Exception:
        print(f'  daemon :{PORT} : arrêté')
    return 0


def _cmd_update():
    """`redhelper update` — force MAINTENANT la mise à jour de helper.py depuis la
    plateforme (ce que le daemon fait tout seul toutes les 6 h), vérifie la
    signature, et l'écrit dans le cache que le shell charge en priorité."""
    api = os.environ.get('REDSTARS_API_BASE', 'https://api.dev.redstars.redlinks.fr')
    print(f"helper.py {VERSION} — recherche d'une version plus récente sur {api}…")
    r = update_self(api)
    if r.get('updated'):
        print(f'  → mis à jour : {r.get("from_version", VERSION)} → {r.get("version")}')
        print('  redémarre le tray/daemon pour charger la nouvelle version.')
        return 0
    if r.get('error'):
        print(f'  échec : {r["error"]}', file=sys.stderr)
        return 1
    print(f'  déjà à jour ({r.get("version", VERSION)}).')
    return 0


def main():
    # Sous-commandes CLI (console / status / update). AVANT tout : on ne démarre
    # ni serveur HTTP ni matériel — ce sont des clients légers, sans interface,
    # pour un terminal. C'est le « autre client » qui n'a pas besoin de GTK.
    sub = sys.argv[1] if len(sys.argv) > 1 else None
    if sub == 'console':
        return run_console(sys.argv[2:])
    if sub == 'status':
        return _cmd_status()
    if sub == 'update':
        return _cmd_update()

    # SO_REUSEADDR : sur Android, l'OS garde le socket 30-120 s en TIME_WAIT
    # après un crash du process. Sans REUSEADDR, le redémarrage de l'app
    # (auto-update du script_updater ou retour foreground) → EADDRINUSE
    # → startupError → MobileBlocker. Idem desktop si le user relance le
    # tray avant la fin du TIME_WAIT.
    HTTPServer.allow_reuse_address = True
    # HTTP server on :PORT — page + helper API, same-origin path.
    #
    # The port can still be held for a beat by the previous helper the
    # single-instance logic just retired (its socket takes a moment to release),
    # or by a live one. Retry with a short backoff instead of crashing every
    # auto-update respawn with a bare `OSError: [Errno 98] Address already in
    # use` traceback (which is exactly what shipped). If it STAYS held, a helper
    # is already serving this port — say so and step aside cleanly, don't die.
    http_srv = None
    for _attempt in range(5):
        try:
            http_srv = HTTPServer(('0.0.0.0', PORT), Handler)
            break
        except OSError as e:
            if e.errno not in (98, 48, 10048):   # not EADDRINUSE (Linux/macOS/Windows)
                raise
            time.sleep(0.3 * (_attempt + 1))
    if http_srv is None:
        print(f'redstars-helper {VERSION}: port {PORT} déjà occupé — '
              f'un helper tourne déjà, rien à démarrer.', file=sys.stderr)
        return
    print(f'redstars-helper {VERSION}')

    # Se tenir à jour, sans qu'on le lui demande.
    _auto_update_loop(os.environ.get('REDSTARS_API_BASE', 'https://api.dev.redstars.redlinks.fr'))

    # Et s'arrêter quand plus aucun site ouvert ne donne signe de vie.
    _start_idle_watchdog()

    print(f'  HTTP  http://0.0.0.0:{PORT}/  +  /helper/*')

    # HTTPS server on :8443 with the local.redlinks.fr cert — required for
    # HTTPS dashboards (dev.redstars.redlinks.fr) to reach /helper/* without
    # mixed-content blocks. local.redlinks.fr is a public DNS A record
    # pointing at 127.0.0.1; the cert is from Let's Encrypt DNS-01.
    https_thread = None
    cert_path, key_path = _resolve_cert_paths()
    if cert_path and key_path and cert_path.is_file() and key_path.is_file():
        ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        ctx.load_cert_chain(certfile=str(cert_path), keyfile=str(key_path))
        # Bind 127.0.0.1 only — the cert is for local.redlinks.fr which always
        # resolves to 127.0.0.1, and we don't want to expose the helper to LAN
        # over HTTPS (the cert/key would let any LAN box impersonate us).
        https_srv = HTTPServer(('127.0.0.1', HTTPS_PORT), Handler)
        https_srv.socket = ctx.wrap_socket(https_srv.socket, server_side=True)
        print(f'  HTTPS https://local.redlinks.fr:{HTTPS_PORT}/  +  /helper/*')
        https_thread = threading.Thread(target=_serve_thread, args=(https_srv, 'HTTPS'), daemon=True)
        https_thread.start()
    else:
        print('  HTTPS: skipped — no cert.pem/key.pem on disk and embedded fallback failed')
    try:
        http_srv.serve_forever(poll_interval=30)  # see _serve_thread: stay dormant when idle
    except KeyboardInterrupt:
        print('\nbye')


if __name__ == '__main__':
    # sys.exit propagates the CLI subcommands' return codes (status/update);
    # the daemon path returns None → exit 0.
    sys.exit(main())
