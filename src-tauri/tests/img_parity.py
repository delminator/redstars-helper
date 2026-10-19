#!/usr/bin/env python3
"""Références Python (img_render.py + onnxruntime) pour la parité du portage Rust src/img/.

Usage :
    python3 tests/img_parity.py OUTDIR [hashes.txt ...]
    IMG_PARITY_DIR=OUTDIR cargo test --release --test img_parity -- --nocapture

Écrit OUTDIR/cases.json :
  - "decode" : pour chaque hash (réels lus dans hashes.txt — un `nn:…` par ligne — plus des
    latents ALÉATOIRES : 1024 / 1026 octets 2 bits et 16384 octets float32), la grille
    d'indices de Python, la marge top1-top2 des logits par pixel (pour juger les écarts
    d'argmax : quasi-égalités ou bug), le temps de décodage, et pour plusieurs tailles de
    tuile les sorties downsample / render_cells / render_ansi_lines / render_tile ;
  - "grids" : des grilles d'indices synthétiques (avec l'indice transparent 255, tailles
    impaires) passées directement à render_cells / render_ansi_lines.
Dépendances : numpy, onnxruntime (venv hors dépôt)."""

import base64
import json
import os
import random
import sys
import time

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "bundled"))
import img_render as R  # noqa: E402

TILES = [(4, 2), (8, 4), (16, 8), (20, 10), (7, 3), (13, 5), (1, 1), (3, 7), (33, 17), (64, 32)]


def cells_json(rows):
    return [[[c["ch"], list(c["fg"]) if c.get("fg") else None, list(c["bg"]) if c.get("bg") else None]
             for c in row] for row in rows]


def main():
    out = sys.argv[1]
    os.makedirs(out, exist_ok=True)
    hashes = []
    for f in sys.argv[2:]:
        hashes += [l.strip() for l in open(f) if l.strip().startswith("nn:")]
    rng = random.Random(1234)
    nrng = np.random.default_rng(1234)
    for n in (1024,) * 12 + (1026,) * 6:
        hashes.append("nn:" + base64.b64encode(bytes(rng.randrange(256) for _ in range(n))).decode())
    for scale in (0.5, 1.0, 1.5, 2.0, 3.0, 1.0, 2.0, 0.25):
        lat = (nrng.standard_normal(4 * 32 * 32) * scale).astype("<f4")
        hashes.append("nn:" + base64.b64encode(lat.tobytes()).decode())

    sess = R._session()
    assert sess is not None, "décodeur Python indisponible"
    name = sess.get_inputs()[0].name
    cases = []
    for h in hashes:
        lat = R.unpack_latent(h)
        t0 = time.perf_counter()
        idx = R.decode_hash(h)
        dt = time.perf_counter() - t0
        logits = sess.run(None, {name: lat})[0].reshape(256, -1)
        top2 = np.partition(logits, -2, axis=0)[-2:]
        margin = (top2[1] - top2[0]).astype(np.float32)
        tiles = []
        for (w, hc) in TILES:
            small = R.downsample_indices(idx, w, hc * 2)
            tiles.append({
                "w": w, "h": hc,
                "small": base64.b64encode(np.asarray(small, np.uint8).tobytes()).decode(),
                "cells": cells_json(R.render_cells(small, w, hc * 2)),
                "ansi": R.render_ansi_lines(small, w, hc * 2),
                "tile": R.render_tile(h, w, hc),
            })
        cases.append({
            "hash": h,
            "latent": base64.b64encode(lat.astype("<f4").tobytes()).decode(),
            "idx": base64.b64encode(idx.tobytes()).decode(),
            "margin": base64.b64encode(margin.tobytes()).decode(),
            "py_ms": dt * 1000,
            "tiles": tiles,
            "full_ansi": R.render_ansi_lines(idx, 256, 256) if len(cases) < 3 else None,
        })
        print(f"{len(cases):3d} décodé en {dt*1000:.0f} ms", file=sys.stderr)

    grids = []
    for k in range(40):
        w, hh = rng.randint(1, 12), rng.randint(1, 9)
        p255 = [0.0, 0.1, 0.5, 0.9][k % 4]
        g = np.array([255 if rng.random() < p255 else rng.randrange(255) for _ in range(w * hh)],
                     np.uint8).reshape(hh, w)
        grids.append({"w": w, "h": hh, "idx": base64.b64encode(g.tobytes()).decode(),
                      "cells": cells_json(R.render_cells(g, w, hh)),
                      "ansi": R.render_ansi_lines(g, w, hh)})

    with open(os.path.join(out, "cases.json"), "w") as f:
        json.dump({"palette": R.PALETTE, "decode": cases, "grids": grids}, f, ensure_ascii=False)
    print(f"{len(cases)} hashes, {len(grids)} grilles → {out}/cases.json", file=sys.stderr)


if __name__ == "__main__":
    main()
