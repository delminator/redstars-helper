"""Décodeur d'images du réseau de rendu, CÔTÉ HELPER.

Le moteur console (frontend) rend déjà n'importe quelle grille d'indices de palette
en caractères texte (demi-blocs / quadrants). Mais les images du réseau — avatars,
vignettes — sont stockées en `nn:` hash (un latent compact) et le DÉCODAGE (hash →
indices) ne tourne que dans le navigateur (ONNX/wasm/WebGPU). La console, elle, rend
côté serveur et s'affiche dans un terminal : elle n'a jamais vu ce décodeur.

Ce module met le décodeur LÀ où l'image doit apparaître : dans le helper, sur la
machine de l'utilisateur. Le helper :
  1. décode le `nn:` hash → indices de palette (onnxruntime + decoder.onnx bundlé),
  2. sous-échantillonne la grille 256×256 vers une petite tuile,
  3. la rend en texte quadrant truecolor — le MÊME algorithme que le moteur TS
     (`icon-image.ts` renderIconImage), porté fidèlement ici,
et le client blitte la tuile dans la trame aux positions marquées par le serveur.

Tout est local : aucune charge serveur, marche hors-ligne et dans le pod restreint.
Le décodage exige onnxruntime + numpy + le modèle ; s'il en manque un, `decode_hash`
renvoie None et l'appelant retombe sur le repli texte (initiales/glyphe).
"""

import base64
import glob
import math
import os


def _r(x):
    """Arrondi identique à JS `Math.round` : au plus proche, 0.5 vers +∞. Python `round`
    fait de l'arrondi bancaire (0.5 → pair), ce qui décale la palette (qui tombe pile sur
    des .5) et les couleurs interpolées — donc la parité avec le moteur TS. floor(x+0.5)
    reproduit exactement Math.round."""
    return int(math.floor(x + 0.5))

# numpy est déjà une dépendance du codec fichiers (codec_numpy). onnxruntime l'est
# aussi (codec_onnx). On importe paresseusement pour que le helper démarre même sans.
try:
    import numpy as _np
except Exception:
    _np = None

# Latent du décodeur d'images : 4 canaux × 32 × 32 (voir nn-decoder.ts LATENT_C/H/W).
LAT_C, LAT_H, LAT_W = 4, 32, 32
DECODE_SIZE = 256          # sortie du modèle : 256×256 indices (nn-decoder DECODE_SIZE)


# ── Palette canonique 4×4×4×4 — IDENTIQUE à buildUniformPalette (décodeur) et à
#    buildPalette (icon-image). Un indice ne veut dire une couleur que si les deux
#    bouts construisent la même table. ────────────────────────────────────────────
def _build_palette():
    p = []
    for r in range(4):
        for g in range(4):
            for b in range(4):
                for l in range(4):
                    s = (l - 1.5) * 25
                    p.append((
                        _r(max(0, min(255, r * 85 + s))),
                        _r(max(0, min(255, g * 85 + s))),
                        _r(max(0, min(255, b * 85 + s))),
                    ))
    return p


PALETTE = _build_palette()   # 256 entrées (r,g,b)
TRANSPARENT = 255


# ── Décodage nn: hash → indices 256×256 ───────────────────────────────────────────

_SESSION = None
_SESSION_TRIED = False
_DECODE_CACHE = {}     # hash -> indices 256×256 (décoder une fois, l'avatar se redessine à chaque touche)
_TILE_CACHE = {}       # (hash, w, h) -> lignes ANSI prêtes à blitter


# Le modèle des avatars a un nom DÉDIÉ : `avatar_decoder.onnx`, PAS le `decoder.onnx`
# déjà bundlé — celui-là sert la page démo et a des poids DIFFÉRENTS (vérifié : ~5 % de
# concordance). Décoder les vrais avatars avec lui donnerait des images fausses en silence.
# Il faut le décodeur PRODUCTION du frontend (public/models/decoder.onnx, celui qui a encodé
# les hashes), livré sous ce nom dédié. Tant qu'il n'est pas là → capable()=False, inerte.
_MODEL_NAME = "avatar_decoder.onnx"


def _find_model():
    """Le décodeur d'avatars, bundlé dans l'app Tauri sous un nom dédié. Cherché aux mêmes
    emplacements que les codecs. Renvoie None s'il manque (repli texte, jamais de faux rendu)."""
    env = os.environ.get("REDSTARS_AVATAR_DECODER_ONNX")
    if env and os.path.isfile(env):
        return env
    pats = [
        "/usr/lib/*/bundled/" + _MODEL_NAME, "/usr/lib/*/resources/bundled/" + _MODEL_NAME,
        "/usr/share/*/bundled/" + _MODEL_NAME, "/opt/*/bundled/" + _MODEL_NAME,
        "/tmp/.mount_*/usr/lib/*/bundled/" + _MODEL_NAME,
        "/Applications/*.app/Contents/Resources/bundled/" + _MODEL_NAME,
        os.path.join(os.path.dirname(__file__), _MODEL_NAME),
    ]
    for pat in pats:
        hits = sorted(glob.glob(pat))
        if hits:
            return hits[0]
    return None


def capable():
    """Le helper peut-il décoder ? Sonde LÉGÈRE — numpy + modèle bundlé + onnxruntime
    présents, SANS lancer d'inférence (pas d'init de session). Sert à la négociation :
    le client n'annonce `img=1` au serveur que si cette sonde passe, pour que le serveur
    ne réserve d'espace-image que là où le client saura le remplir."""
    if _np is None:
        return False
    if _find_model() is None:
        return False
    try:
        import onnxruntime  # noqa: F401
        return True
    except Exception:
        return False


def _session():
    global _SESSION, _SESSION_TRIED
    if _SESSION_TRIED:
        return _SESSION
    _SESSION_TRIED = True
    model = _find_model()
    if not model or _np is None:
        return None
    try:
        import onnxruntime as ort
        so = ort.SessionOptions()
        so.intra_op_num_threads = max(1, (os.cpu_count() or 2) - 1)
        _SESSION = ort.InferenceSession(model, so, providers=["CPUExecutionProvider"])
    except Exception:
        _SESSION = None
    return _SESSION


def unpack_latent(b64hash):
    """`nn:<base64>` → latent float32 (1,4,32,32). Deux formats, comme unpackLatent :
      - 16384 octets : float32 brut (sortie encodeur) ;
      - 1024 octets : legacy 2 bits/valeur, niveaux {-3,-1,1,3}."""
    b64 = b64hash[3:] if b64hash.startswith("nn:") else b64hash
    raw = base64.b64decode(b64)
    total = LAT_C * LAT_H * LAT_W
    if len(raw) == total * 4:
        arr = _np.frombuffer(raw, dtype="<f4").astype(_np.float32)
    else:
        # 2 bits par valeur, 4 valeurs par octet, MSB d'abord : idx*2-3 ∈ {-3,-1,1,3}.
        vals = _np.empty(total, dtype=_np.float32)
        for i in range(min(1024, len(raw))):
            byte = raw[i]
            vals[i * 4]     = ((byte >> 6) & 3) * 2 - 3
            vals[i * 4 + 1] = ((byte >> 4) & 3) * 2 - 3
            vals[i * 4 + 2] = ((byte >> 2) & 3) * 2 - 3
            vals[i * 4 + 3] = (byte & 3) * 2 - 3
        arr = vals
    return arr.reshape(1, LAT_C, LAT_H, LAT_W)


def decode_hash(b64hash):
    """`nn:` hash → indices de palette uint8 (256,256), ou None si le décodeur manque.
    Mis en cache : un avatar se redessine à chaque touche, on ne décode qu'une fois."""
    if b64hash in _DECODE_CACHE:
        return _DECODE_CACHE[b64hash]
    sess = _session()
    if sess is None:
        return None
    try:
        latent = unpack_latent(b64hash)
        name = sess.get_inputs()[0].name
        out = sess.run(None, {name: latent})[0]        # (1,256,256,256) logits, classes-first
        logits = _np.asarray(out).reshape(256, DECODE_SIZE * DECODE_SIZE)
        idx = logits.argmax(axis=0).astype(_np.uint8)  # argmax sur la classe → indice
        idx = idx.reshape(DECODE_SIZE, DECODE_SIZE)
        _DECODE_CACHE[b64hash] = idx
        return idx
    except Exception:
        return None


def render_tile(b64hash, w_cells, h_cells):
    """`nn:` hash → lignes ANSI d'une tuile de w_cells × h_cells cellules (prêtes à blitter),
    ou None si le décodeur manque. Décodage + downsample + rendu mis en cache par (hash,w,h)."""
    key = (b64hash, w_cells, h_cells)
    if key in _TILE_CACHE:
        return _TILE_CACHE[key]
    idx = decode_hash(b64hash)
    if idx is None:
        return None
    # Une cellule = 2 sous-pixels de haut : pour h_cells lignes il faut 2*h_cells pixels.
    small = downsample_indices(idx, w_cells, h_cells * 2)
    lines = render_ansi_lines(small, w_cells, h_cells * 2)
    _TILE_CACHE[key] = lines
    return lines


# ── Sous-échantillonnage vers une petite tuile (moyenne en RGB, re-quantifié) ──────

def _nearest_index(r, g, b):
    best, bd = 0, 1 << 30
    for i, (pr, pg, pb) in enumerate(PALETTE):
        d = (pr - r) ** 2 + (pg - g) ** 2 + (pb - b) ** 2
        if d < bd:
            bd, best = d, i
    return best


def downsample_indices(idx2d, dw, dh):
    """indices (H,W) → petite grille d'indices (dh,dw). Moyenne par bloc EN RGB (moyenner
    des indices n'a pas de sens), puis re-quantifié sur la palette. Les images du réseau
    sont opaques : pas de transparence à gérer ici."""
    H = len(idx2d) if not hasattr(idx2d, "shape") else idx2d.shape[0]
    W = (len(idx2d[0]) if not hasattr(idx2d, "shape") else idx2d.shape[1])
    if _np is not None and hasattr(idx2d, "shape"):
        pal = _np.asarray(PALETTE, dtype=_np.float32)          # (256,3)
        rgb = pal[idx2d]                                        # (H,W,3)
        out = _np.empty((dh, dw), dtype=_np.uint8)
        ys = (_np.arange(dh + 1) * H // dh)
        xs = (_np.arange(dw + 1) * W // dw)
        for j in range(dh):
            for i in range(dw):
                block = rgb[ys[j]:max(ys[j] + 1, ys[j + 1]), xs[i]:max(xs[i] + 1, xs[i + 1])]
                mr, mg, mb = block.reshape(-1, 3).mean(axis=0)
                out[j, i] = _nearest_index(round(mr), round(mg), round(mb))
        return out
    # Repli pur-python (sans numpy) — rarement pris, mais garde le module autonome.
    out = [[0] * dw for _ in range(dh)]
    for j in range(dh):
        for i in range(dw):
            y0, y1 = j * H // dh, max(j * H // dh + 1, (j + 1) * H // dh)
            x0, x1 = i * W // dw, max(i * W // dw + 1, (i + 1) * W // dw)
            sr = sg = sb = n = 0
            for y in range(y0, y1):
                for x in range(x0, x1):
                    pr, pg, pb = PALETTE[idx2d[y][x]]
                    sr += pr; sg += pg; sb += pb; n += 1
            out[j][i] = _nearest_index(round(sr / n), round(sg / n), round(sb / n))
    return out


# ── Rendu quadrant → texte ANSI truecolor — PORT FIDÈLE de icon-image.ts ───────────

# 16 glyphes quadrant, indexés par un masque 4 bits (TL=1, TR=2, BL=4, BR=8).
_QUAD = [' ', '▘', '▝', '▀', '▖', '▌', '▞', '▛', '▗', '▚', '▐', '▜', '▄', '▙', '▟', '█']
_FLAT = 24 * 24 * 3     # en-deçà de cet écart, le bloc 2×2 est une seule couleur


def _px(idx2d, w, h, x, y):
    if x < 0 or x >= w or y < 0 or y >= h:
        return None
    k = idx2d[y][x] if not hasattr(idx2d, "shape") else int(idx2d[y, x])
    return None if k == TRANSPARENT else PALETTE[k]


def _half(idx2d, w, h, x, y):
    a = _px(idx2d, w, h, x, y)
    b = _px(idx2d, w, h, x + 1, y)
    if a and b:
        return (_r((a[0] + b[0]) / 2), _r((a[1] + b[1]) / 2), _r((a[2] + b[2]) / 2))
    return a if a else b


def _avg(cs):
    n = len(cs)
    return (_r(sum(c[0] for c in cs) / n),
            _r(sum(c[1] for c in cs) / n),
            _r(sum(c[2] for c in cs) / n))


def _d2(a, b):
    return (a[0] - b[0]) ** 2 + (a[1] - b[1]) ** 2 + (a[2] - b[2]) ** 2


def render_cells(idx2d, w, h):
    """Grille d'indices (h,w) → lignes de cellules {ch, fg, bg} (RGB exact). `w` cellules
    de large, ceil(h/2) de haut : mêmes dimensions que le renderer TS."""
    rows = []
    y = 0
    while y < h:
        row = []
        for x in range(w):
            sub = [_px(idx2d, w, h, x, y), _half(idx2d, w, h, x, y),
                   _px(idx2d, w, h, x, y + 1), _half(idx2d, w, h, x, y + 1)]
            mask = 0
            opaque = []
            for i in range(4):
                if sub[i] is not None:
                    mask |= 1 << i
                    opaque.append(sub[i])
            if mask == 0:
                row.append({'ch': ' '})
                continue
            if mask != 0b1111:
                c = _avg(opaque)
                row.append({'ch': _QUAD[mask], 'fg': c})
                continue
            # 4 sous-pixels opaques → 2 couleurs (pôles = paire la plus éloignée).
            p0, p1, bd = 0, 1, -1
            for i in range(4):
                for j in range(i + 1, 4):
                    d = _d2(sub[i], sub[j])
                    if d > bd:
                        bd, p0, p1 = d, i, j
            if bd < _FLAT:
                c = _avg(opaque)
                row.append({'ch': '█', 'fg': c})
                continue
            fg_mask = 0
            fg_cols, bg_cols = [], []
            for i in range(4):
                if _d2(sub[i], sub[p0]) <= _d2(sub[i], sub[p1]):
                    fg_mask |= 1 << i
                    fg_cols.append(sub[i])
                else:
                    bg_cols.append(sub[i])
            row.append({'ch': _QUAD[fg_mask], 'fg': _avg(fg_cols), 'bg': _avg(bg_cols)})
        rows.append(row)
        y += 2
    return rows


def render_ansi_lines(idx2d, w, h):
    """Grille d'indices → lignes ANSI truecolor autonomes (chaque ligne se referme par
    un reset). Pour un blit ligne à ligne à une position curseur donnée."""
    out = []
    for row in render_cells(idx2d, w, h):
        buf = []
        cur = None
        for cell in row:
            fg, bg = cell.get('fg'), cell.get('bg')
            if fg is None and bg is None:
                code = None
            else:
                parts = []
                if fg is not None:
                    parts.append("38;2;%d;%d;%d" % fg)
                if bg is not None:
                    parts.append("48;2;%d;%d;%d" % bg)
                code = ";".join(parts)
            if code != cur:
                buf.append("\x1b[0m" if code is None else "\x1b[" + code + "m")
                cur = code
            buf.append(cell['ch'])
        buf.append("\x1b[0m")
        out.append("".join(buf))
    return out
