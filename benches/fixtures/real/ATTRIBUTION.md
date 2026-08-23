# Real-photo benchmark fixtures — provenance and licence

Both files in this directory are re-encoded, downscaled derivatives of
NASA photographs. NASA still-image content is a work of the U.S. federal
government and is public domain in the United States (no copyright, per
17 U.S.C. §105); Wikimedia Commons independently tags both source files
`{{PD-USGov-NASA}}`/`Public domain`, confirmed via the Commons API
(`action=query&prop=imageinfo&iiprop=extmetadata`) before downloading —
see the exact JSON captured for each file below. Neither file is the
Kodak True Color corpus `adr/0003`/`adr/0004` used for their scratch-crate
measurements — the project owner flagged that Kodak's own licence is not
established as permissive enough to commit into this repository, so a
different, unambiguously public-domain source was used instead.

## `blue-marble.jpg`

- **Source:** "Blue Marble 2002" — NASA Earth Observatory / Reto Stöckli,
  NASA GSFC. Composite Earth imagery (MODIS), chosen for realistic cloud/
  ocean/land texture — the kind of broadband photographic detail
  `gradient_noise_rgb`'s i.i.d. per-pixel noise does not reproduce.
- **Wikimedia Commons page:** `https://commons.wikimedia.org/wiki/File:Blue_Marble_2002.png`
- **Master file:** 43200×21600 PNG. Downloaded via Wikimedia's own
  thumbnailing endpoint at 3840px wide (`.../thumb/2/23/Blue_Marble_2002.png/3840px-Blue_Marble_2002.png`,
  itself a lossless PNG re-sample of the master, not a re-compression) so
  only a scaled-down copy was ever transferred.
- **Local processing (this repo only, not run at bench time):** resized
  3840×1920 → 2200×1100 (Lanczos3, Pillow) — comfortably ≥ every fixture
  size this benchmark suite requests (max 1920×1080) so every derived
  fixture is a pure downscale, never an upscale — then re-encoded as
  JPEG quality 85 (Pillow, `optimize=True`). 294,007 bytes.
- **Commons licence tag (`LicenseShortName`):** `Public domain`.

## `earthrise.jpg`

- **Source:** "Earthrise" (AS8-14-2383HR) — NASA/Bill Anders, Apollo 8,
  24 December 1968. Chosen as a second, structurally different real photo:
  near-black space, high-texture lunar regolith, and a small high-detail
  Earth — content the single Blue Marble image doesn't cover, so a
  synthetic-vs-real comparison isn't resting on one image's particular
  compressibility.
- **Wikimedia Commons page:** `https://commons.wikimedia.org/wiki/File:NASA-Apollo8-Dec24-Earthrise.jpg`
- **Master file:** 2400×2400 JPEG, downloaded directly (no thumbnailing
  needed at that size).
- **Local processing:** resized 2400×2400 → 1280×1280 (Lanczos3, Pillow),
  re-encoded JPEG quality 85. 80,208 bytes.
- **Commons licence tag (`LicenseShortName`):** `Public domain`.

## Verifying the licence tags yourself

```
curl -s -A 'some-UA/1.0' \
  'https://commons.wikimedia.org/w/api.php?action=query&titles=File:Blue_Marble_2002.png&prop=imageinfo&iiprop=extmetadata&format=json' \
  | python3 -c "import json,sys; d=json.load(sys.stdin); print(list(d['query']['pages'].values())[0]['imageinfo'][0]['extmetadata']['LicenseShortName']['value'])"
# -> Public domain
```

Same query with `titles=File:NASA-Apollo8-Dec24-Earthrise.jpg` returns the
same value.

## Total size

368 KB combined (294 KB + 80 KB) — well under the 1 MB budget discussed for
this change. `benches/fixtures.rs` loads both via `include_bytes!` at
compile time; no benchmark run performs a network fetch.
