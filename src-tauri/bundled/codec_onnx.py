#!/usr/bin/env python3
"""Codec per-bloc LOSSLESS GARANTI — inférence ONNX (onnxruntime, sans torch).

Même sémantique/format que codec_numpy (MAGIC RSN1 + sidecar de correction)
mais l'inférence des nets 32×32 passe par enc32.onnx / dec32.onnx via
onnxruntime, multi-thread (tous les cœurs) → ~×100 plus rapide que nn_numpy.

Format : MAGIC(4) | orig_len u64 BE | latents | trailer(n_patches u32 + (off u64,octet)×N)
"""
import os
import numpy as np
import onnxruntime as ort

_DIR = os.path.dirname(os.path.abspath(__file__))
BLOCK = 1024
MAGIC = b"RSN1"
HEADER = 12

_so = ort.SessionOptions()
_so.intra_op_num_threads = os.cpu_count() or 4          # parallélise chaque inférence sur tous les cœurs
_so.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_ALL
_ENC = ort.InferenceSession(os.path.join(_DIR, "enc32.onnx"), _so, providers=["CPUExecutionProvider"])
_DEC = ort.InferenceSession(os.path.join(_DIR, "dec32.onnx"), _so, providers=["CPUExecutionProvider"])


def _to_bits(idx):                                       # (B,32,32) uint8 -> (B,8,32,32) f32
    bits = np.arange(8, dtype=np.int64)[None, :, None, None]
    return ((idx.astype(np.int64)[:, None, :, :] >> bits) & 1).astype(np.float32)


def _enc(patches):                                       # (B,32,32) uint8 -> (B,8,32,32) uint8 bits
    return _ENC.run(["latbits"], {"bits": _to_bits(patches)})[0].astype(np.uint8)


def _dec(z):                                             # (B,8,32,32) -> (B,32,32) uint8
    return _DEC.run(["idx"], {"latbits": z.astype(np.float32)})[0].astype(np.uint8)


def _enc_blocks(raw):
    z = _enc(raw.reshape(-1, 32, 32))
    return z, np.packbits(z.reshape(len(z), -1), axis=1).tobytes()


def _dec_bytes(lat_bytes):
    lat = np.frombuffer(lat_bytes, np.uint8).reshape(-1, BLOCK)
    z = np.unpackbits(lat, axis=1).reshape(-1, 8, 32, 32)
    return _dec(z).reshape(-1)


def _trailer(patches):
    out = bytearray(len(patches).to_bytes(4, "big"))
    for off, b in patches:
        out += off.to_bytes(8, "big") + bytes([b])
    return bytes(out)


def encode(data: bytes) -> bytes:
    orig = np.frombuffer(data, np.uint8)
    n = len(orig)
    pad = (-n) % BLOCK
    raw = np.concatenate([orig, np.zeros(pad, np.uint8)]) if pad else orig
    z, lat = _enc_blocks(raw)
    back = _dec(z).reshape(-1)[:n]
    patches = [(int(o), int(orig[o])) for o in np.nonzero(back != orig)[0]]
    return MAGIC + n.to_bytes(8, "big") + lat + _trailer(patches)


def decode(blob: bytes) -> bytes:
    assert blob[:4] == MAGIC, "format inconnu"
    n = int.from_bytes(blob[4:HEADER], "big")
    nblocks = (n + BLOCK - 1) // BLOCK
    out = bytearray(_dec_bytes(blob[HEADER:HEADER + nblocks * BLOCK])[:n].tobytes())
    tr = blob[HEADER + nblocks * BLOCK:]
    npatch = int.from_bytes(tr[:4], "big")
    pos = 4
    for _ in range(npatch):
        out[int.from_bytes(tr[pos:pos + 8], "big")] = tr[pos + 8]
        pos += 9
    return bytes(out)


def encode_file(in_path, out_path, batch: int = 512):
    n = os.path.getsize(in_path)
    patches, goff = [], 0
    with open(in_path, "rb") as fi, open(out_path, "wb") as fo:
        fo.write(MAGIC + n.to_bytes(8, "big"))
        while True:
            chunk = fi.read(BLOCK * batch)
            if not chunk:
                break
            orig = np.frombuffer(chunk, np.uint8)
            clen = len(orig)
            pad = (-clen) % BLOCK
            raw = np.concatenate([orig, np.zeros(pad, np.uint8)]) if pad else orig
            z, lat = _enc_blocks(raw)
            fo.write(lat)
            back = _dec(z).reshape(-1)[:clen]
            for idx in np.nonzero(back != orig)[0]:
                patches.append((goff + int(idx), int(orig[idx])))
            goff += clen
        fo.write(_trailer(patches))
    return n, len(patches)


def decode_file(in_path, out_path, batch: int = 512):
    with open(in_path, "rb") as fi:
        head = fi.read(HEADER)
        n = int.from_bytes(head[4:HEADER], "big")
        nblocks = (n + BLOCK - 1) // BLOCK
        body_size = nblocks * BLOCK
        read = written = 0
        with open(out_path, "wb") as fo:
            while read < body_size:
                want = min(BLOCK * batch, body_size - read)
                chunk = fi.read(want)
                if not chunk:
                    break
                read += len(chunk)
                raw = _dec_bytes(chunk).tobytes()
                rem = n - written
                if len(raw) > rem:
                    raw = raw[:rem]
                fo.write(raw)
                written += len(raw)
        tr = fi.read()
    npatch = int.from_bytes(tr[:4], "big")
    if npatch:
        pos = 4
        with open(out_path, "r+b") as f:
            for _ in range(npatch):
                f.seek(int.from_bytes(tr[pos:pos + 8], "big"))
                f.write(bytes([tr[pos + 8]]))
                pos += 9
    return n


if __name__ == "__main__":
    import sys, time, hashlib
    if len(sys.argv) == 4:
        inp, blob, out = sys.argv[1:4]
        t = time.time(); _, np_ = encode_file(inp, blob); te = time.time() - t
        t = time.time(); decode_file(blob, out); td = time.time() - t
        ha = hashlib.sha256(open(inp, "rb").read()).hexdigest()
        hb = hashlib.sha256(open(out, "rb").read()).hexdigest()
        mb = os.path.getsize(inp) / 1048576
        print(f"threads={_so.intra_op_num_threads}  patches={np_}")
        print(f"encode+verify {te:.1f}s ({mb/te:.0f} Mo/s)  decode {td:.1f}s ({mb/td:.0f} Mo/s)")
        print("LOSSLESS ✓" if ha == hb else "PERTE ✗")
    else:
        for sz in (1024, 100000):
            d = os.urandom(sz)
            print(sz, "->", "LOSSLESS" if decode(encode(d)) == d else "PERTE")
