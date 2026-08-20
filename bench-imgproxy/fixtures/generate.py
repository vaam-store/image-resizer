#!/usr/bin/env python3
"""Deterministic fixture corpus generator for the emgr vs. imgproxy benchmark.

Why generated instead of downloaded: the harness must not depend on the
public internet at benchmark time (see bench-imgproxy/README.md). The
project's own Rust-side fixtures (benches/fixtures.rs) already use the same
approach -- a fixed RNG seed producing byte-identical output on every
machine -- for exactly this reason. This script is the equivalent for the
benchmark harness, written in Python (Pillow) instead of Rust because
bench-imgproxy/ is not allowed to touch anything under src/ or Cargo.toml,
and the corpus needs to exist as static files an nginx origin container can
serve, not as in-process Rust bytes.

Run once, from a clean checkout, before docker compose up:

    python3 -m venv .venv && source .venv/bin/activate
    pip install pillow numpy
    python3 generate.py

Output goes to fixtures/corpus/ and is what the "origin" nginx service
serves over HTTP during a benchmark run. Re-running this script regenerates
byte-identical files (same seed, same algorithm), so the corpus does not
need to be committed to get a reproducible run -- but nothing stops you
from committing it either, if you'd rather not require Python at
benchmark-setup time on every machine that runs the harness.
"""

from __future__ import annotations

import pathlib

import numpy as np
from PIL import Image

SEED = 0x1BAD_1DEA_C0FF_EE42
OUT_DIR = pathlib.Path(__file__).parent / "corpus"


def rng_for(label: str) -> np.random.Generator:
    """Mix `label` into the base seed, matching benches/fixtures.rs's
    rng_for() so different fixtures don't share a pixel stream while
    staying fully deterministic per label."""
    mix = SEED
    for byte in label.encode():
        mix = ((mix << 5) | (mix >> 59)) & 0xFFFFFFFFFFFFFFFF
        mix ^= byte
    return np.random.default_rng(mix)


def gradient_noise_rgb(width: int, height: int, label: str) -> Image.Image:
    """Smooth gradient + per-pixel noise -- compresses like a real photo,
    not a flat colour. Mirrors benches/fixtures.rs's gradient_noise_rgb()."""
    rng = rng_for(label)
    xs = np.linspace(0, 255, width, dtype=np.float32)
    ys = np.linspace(0, 255, height, dtype=np.float32)
    gx = np.tile(xs, (height, 1))
    gy = np.tile(ys.reshape(-1, 1), (1, width))
    noise = rng.integers(-24, 25, size=(height, width)).astype(np.float32)

    r = np.clip(gx + noise, 0, 255)
    g = np.clip(gy + noise, 0, 255)
    b = np.clip((gx + gy) / 2 + noise, 0, 255)

    arr = np.stack([r, g, b], axis=-1).astype(np.uint8)
    return Image.fromarray(arr, mode="RGB")


def flat_rgb(width: int, height: int) -> Image.Image:
    """A single solid colour -- compresses to almost nothing. Matches
    benches/fixtures.rs's solid_rgb() colour exactly (#248CD2)."""
    arr = np.zeros((height, width, 3), dtype=np.uint8)
    arr[:, :] = (36, 140, 210)
    return Image.fromarray(arr, mode="RGB")


def alpha_fringe_rgba(size: int, label: str) -> Image.Image:
    """RGBA image with a transparent border whose RGB channels are garbage
    (non-zero). Exercises the PNG -> JPEG/WebP flattening path: naive alpha
    handling bleeds the garbage colour into the visible border. Mirrors
    benches/fixtures.rs's alpha_fringe_rgba()."""
    rng = rng_for(label)
    border = max(size // 16, 4)

    arr = np.zeros((size, size, 4), dtype=np.uint8)
    xs = np.arange(size)
    ys = np.arange(size)
    gx = (xs.astype(np.float32) / size * 255).astype(np.uint8)
    gy = (ys.astype(np.float32) / size * 255).astype(np.uint8)

    arr[:, :, 0] = np.tile(gx, (size, 1))
    arr[:, :, 1] = np.tile(gy.reshape(-1, 1), (1, size))
    arr[:, :, 2] = 200
    arr[:, :, 3] = 255

    on_border = np.zeros((size, size), dtype=bool)
    on_border[:border, :] = True
    on_border[-border:, :] = True
    on_border[:, :border] = True
    on_border[:, -border:] = True

    garbage = rng.integers(0, 256, size=(size, size, 3)).astype(np.uint8)
    arr[on_border, 0:3] = garbage[on_border]
    arr[on_border, 3] = 0

    return Image.fromarray(arr, mode="RGBA")


def main() -> None:
    OUT_DIR.mkdir(parents=True, exist_ok=True)

    photos = {
        "photo_4k.jpg": (3840, 2160),
        "photo_1080p.jpg": (1920, 1080),
        "photo_800x600.jpg": (800, 600),
    }
    for name, (w, h) in photos.items():
        img = gradient_noise_rgb(w, h, f"photo_{w}x{h}")
        path = OUT_DIR / name
        # quality=90, no subsampling override -- a plausible "already
        # compressed camera JPEG" rather than a pathologically clean
        # synthetic source, so both proxies pay a realistic decode cost.
        img.save(path, format="JPEG", quality=90)
        print(f"wrote {path} ({path.stat().st_size} bytes, {w}x{h})")

    alpha_img = alpha_fringe_rgba(1024, "alpha_fringe_1024")
    alpha_path = OUT_DIR / "alpha_1024.png"
    alpha_img.save(alpha_path, format="PNG")
    print(f"wrote {alpha_path} ({alpha_path.stat().st_size} bytes, 1024x1024, RGBA)")

    flat_img = flat_rgb(1024, 1024)
    flat_path = OUT_DIR / "flat_1024.png"
    flat_img.save(flat_path, format="PNG", optimize=True)
    print(f"wrote {flat_path} ({flat_path.stat().st_size} bytes, 1024x1024, RGB)")

    print("\nCorpus generation complete. Files are byte-identical on every")
    print("machine for this script version (fixed seed, no wall-clock or")
    print("hostname inputs).")


if __name__ == "__main__":
    main()
