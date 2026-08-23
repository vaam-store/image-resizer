# Usage Examples

Practical examples against the real, signed-URL API (see the
[API reference](api-reference.md) for the full grammar, the pipeline
order, and every option's divergence from imgproxy). Every URL below is a
**genuine, working signature** — computed with the Python snippet under
[Computing a signature](#computing-a-signature) against the same
placeholder key/salt as the API reference's worked example:
`SIGNING_KEY=6d792d7369676e696e672d6b6579`,
`SIGNING_SALT=6d792d73616c74` (hex for `my-signing-key` / `my-salt`) —
never use these for anything real. Every example resizes
`https://images.example.com/photo.jpg` unless noted otherwise.

!!! note "The resize endpoint redirects, it doesn't stream bytes back"
    `GET /{signature}/{options}/{source}.{extension}` responds
    `301 Moved Permanently` with a `Location` header pointing at the
    resized image, rather than returning the image bytes directly.
    Browsers, `fetch()`, and `curl -L` all follow that redirect
    transparently, so most of the examples below work unmodified — just be
    aware a plain `curl` (no `-L`) will save the (empty) redirect
    response, not an image.

!!! note "Every request needs a real signature"
    A request with a missing, wrong, or (unless `ALLOW_UNSIGNED_REQUESTS=true`
    is set) `unsigned` signature gets `403 Forbidden`, never processed. See
    [Signing and fail-closed startup](api-reference.md#signature) in the
    API reference for the exact HMAC scheme.

## Computing a signature

```python
import hashlib
import hmac
import base64

def sign(key_hex: str, salt_hex: str, path: str) -> str:
    key = bytes.fromhex(key_hex)
    salt = bytes.fromhex(salt_hex)
    mac = hmac.new(key, salt + path.encode(), hashlib.sha256).digest()
    return base64.urlsafe_b64encode(mac).rstrip(b"=").decode()

def encode_source(url: str) -> str:
    return base64.urlsafe_b64encode(url.encode()).rstrip(b"=").decode()

key_hex = "6d792d7369676e696e672d6b6579"
salt_hex = "6d792d73616c74"

source = encode_source("https://images.example.com/photo.jpg")
path = f"/rs:fit:200:200/q:80/{source}.webp"
signature = sign(key_hex, salt_hex, path)

print(f"/{signature}{path}")
```

## Common cases

### Thumbnail

```
GET /USj4F2ERoKKugAeQ54JQct8oGudbkUzGYdIuJncZawk/rs:fit:200:200/q:80/aHR0cHM6Ly9pbWFnZXMuZXhhbXBsZS5jb20vcGhvdG8uanBn.webp
```

Resizes to fit inside a 200x200 box (aspect ratio preserved, so the
output may be narrower or shorter than 200 on one axis), at quality 80,
encoded as WebP.

### Fill-crop with gravity

```
GET /GsN7dLZQjyIq4gDj5iKQBZvL2HZu63AZd7o5vRU8pJM/rs:fill:400:300/gr:no/aHR0cHM6Ly9pbWFnZXMuZXhhbXBsZS5jb20vcGhvdG8uanBn.jpg
```

Scales to *cover* 400x300, then crops the overflow anchored to the
**north** edge (`gr:no`) instead of the default centre — useful when the
interesting part of a photo (a face, a headline) sits near the top and a
centred crop would cut it off.

**Note the code is `gr:`, not `g:`.** This service's `g:` means grayscale,
not gravity — see [the callout in the API reference](api-reference.md#the-most-important-divergence-from-imgproxy-g-is-grayscale-not-gravity)
for why, and the [gravity migration example](#migrating-an-imgproxy-gravity-url)
below for what happens if you use imgproxy's own `g:` by mistake.

### Format conversion

Convert to AVIF (smallest, most modern; both encode and decode are
supported — see the [format table](api-reference.md#plainbase64-sourceextension)):

```
GET /X20ne2Igk1DfeVV6z5WxY5Be4X-Vbei4l55EfcDN1wE/q:70/aHR0cHM6Ly9pbWFnZXMuZXhhbXBsZS5jb20vcGhvdG8uanBn.avif
```

Convert to WebP with no other processing:

```
GET /V6QMdiu1wWZPyU7xF2lOlAVwzjUAZ_gK74g1N_InZ6Q/aHR0cHM6Ly9pbWFnZXMuZXhhbXBsZS5jb20vcGhvdG8uanBn.webp
```

The trailing extension is always the output format, independent of the
source's own extension or content type.

### Quality control

```
GET /8rPY_ywRdyK5SJ0fkdkxHE4oJUYFZ7aHPEFdzWjebtE/rs:fit:1200:0/mb:150000/aHR0cHM6Ly9pbWFnZXMuZXhhbXBsZS5jb20vcGhvdG8uanBn.jpg
```

Resizes to fit within 1200px width (height `0` means "not set" — aspect
ratio preserved), then encodes JPEG with quality iteratively lowered until
the output is under 150,000 bytes (`mb:150000`). `mb:` only works for
JPEG output here — see the [`mb:` row](api-reference.md#simple-options)
in the API reference for why WebP/PNG/AVIF/GIF ignore it.

### Watermarking

```
GET /cGTrlLki-lIUwMLHKqrTPQzEbtGjpxY3NG6yZ4Yh1Zc/rs:fit:800:600/wm:0.6:soea:16:16:0.2/aHR0cHM6Ly9pbWFnZXMuZXhhbXBsZS5jb20vcGhvdG8uanBn.jpg
```

Composites this deployment's default watermark image (`WATERMARK_URL` —
see [Configuration](../getting-started/configuration.md)) at 60% opacity,
anchored to the south-east corner with a 16px offset on each axis, scaled
to 20% of the base image's size. To use a different, per-request
watermark image instead of the deployment default, add `wmu:{base64url
watermark URL}`. **Only image watermarks are supported** — there is no
text-watermarking option in this grammar at all.

### Presets

Requires `PRESETS=thumbnail=rs:fill:300:300/q:80` (or similar) configured
on the server — see [Presets and the allowlist](api-reference.md#presets-pr-and-the-processing-option-allowlist).

```
GET /_K8QuPpYQSz_HDfhaUaWE5Intcf5K0dBqP2MSgEWRK8/pr:thumbnail/aHR0cHM6Ly9pbWFnZXMuZXhhbXBsZS5jb20vcGhvdG8uanBn.jpg
```

Expands to exactly `rs:fill:300:300/q:80` server-side. A request can still
override part of what a preset sets, since segments apply left to right:

```
GET /i7IX-wbqFpioTVJYdJtAIEmu2lyH6ERZSOfZEFoIsec/pr:thumbnail/q:95/aHR0cHM6Ly9pbWFnZXMuZXhhbXBsZS5jb20vcGhvdG8uanBn.jpg
```

Same 300x300 fill crop, but quality 95 instead of the preset's 80.

### `.auto` content negotiation

```
GET /PxkE0dXMg8tHKsJKnbMkrUrA1tDnqVmjJvUGNyh8ji0/rs:fit:800:0/aHR0cHM6Ly9pbWFnZXMuZXhhbXBsZS5jb20vcGhvdG8uanBn.auto
```

`.auto` isn't a real output format — it's resolved against the request's
`Accept` header before the image is ever processed
(`crate::modules::negotiation::resolve`): AVIF if the client advertises
it, else WebP, else JPEG, weighted by each entry's `q` parameter. The
response carries `Vary: Accept` so caches don't serve a negotiated result
to a client that asked for something different:

```bash
curl -L -o photo.avif \
  -H 'Accept: image/avif,image/webp,image/*;q=0.8' \
  "https://your-service.com/PxkE0dXMg8tHKsJKnbMkrUrA1tDnqVmjJvUGNyh8ji0/rs:fit:800:0/aHR0cHM6Ly9pbWFnZXMuZXhhbXBsZS5jb20vcGhvdG8uanBn.auto"
```

An explicit `.jpg`/`.png`/`.webp`/`.avif`/`.gif` extension is never
affected by `Accept` — negotiation only ever happens for `.auto`.

## Migrating an imgproxy gravity URL

An imgproxy URL that anchors a fill crop with `g:no` (north gravity) does
**not** silently produce a wrong-but-plausible result here — `g:` takes
exactly one boolean argument in this service, and `no` fails to parse as
one, so the request is rejected outright:

```
GET /{signature}/rs:fill:400:300/g:no/{base64 source}.jpg
→ 400 Bad Request: invalid value for processing option "g:no"
```

The fix is to rewrite `g:` to `gr:` for gravity, keeping `g:` reserved for
grayscale:

```
GET /BBpe0KuAM-evlv6Imxy44EEt5Xk5gVRyIjaTL5nKKf0/gr:no/aHR0cHM6Ly9pbWFnZXMuZXhhbXBsZS5jb20vcGhvdG8uanBn.jpg
```

## More processing options

### Grayscale

```
GET /7Ydb5QQ5ZSOybTjD0wfHE6yBT-kpDSgd4xXZ9EwDhfc/g:true/aHR0cHM6Ly9pbWFnZXMuZXhhbXBsZS5jb20vcGhvdG8uanBn.jpg
```

### Explicit crop

```
GET /LdpbqOKHQgoBoq6W4hx_NEPgHNWyyHJFKCKoNbn5s6s/c:0.5:0.5:noea/aHR0cHM6Ly9pbWFnZXMuZXhhbXBsZS5jb20vcGhvdG8uanBn.jpg
```

Crops to 50% of the source's width and height (`0.5` is a relative
fraction, since it's under `1.0`), anchored to the north-east corner.

### Trim and padding

```
GET /iBTsc8J02JiEo6WN4oFMrFk8wGsFWvsJk7DZDtlmcH8/t:10/pd:20/aHR0cHM6Ly9pbWFnZXMuZXhhbXBsZS5jb20vcGhvdG8uanBn.png
```

Trims uniform-colour borders (threshold `10`, auto-detected from the
top-left pixel), then pads the trimmed image by 20px on every side
(`pd:20` — CSS-shorthand style, so a single value applies to all four
sides).

### Rotate and flip

```
GET /NeWEj5rX0EiNuc4olyJKkhqgxIfmSlpze7z0TPBLeJA/rot:90/fl:1:0/aHR0cHM6Ly9pbWFnZXMuZXhhbXBsZS5jb20vcGhvdG8uanBn.jpg
```

Rotates 90° clockwise, then flips horizontally only. Both run *after*
resize — see [the pipeline order diagram](api-reference.md#processing-pipeline-order)
in the API reference.

### Responsive images with `dpr`

```
GET /8xN0tTepw8WnOinCA-oAbHMDBfIw5plK1bW7orSS87E/rs:fit:300:0/dpr:2/aHR0cHM6Ly9pbWFnZXMuZXhhbXBsZS5jb20vcGhvdG8uanBn.jpg
```

Requests a 300 CSS-px-wide slot on a 2x-density screen — the actual
resized output is 600px wide. `dpr:` (like `z:`) only scales an axis that
already has an explicit width or height set.

### JPEG tuning

```
GET /Ak3BWWUBgmlzDcM-GVlhHk4aiUOMNx6jBPKXIjsUHPM/jpgo:1:1/q:85/aHR0cHM6Ly9pbWFnZXMuZXhhbXBsZS5jb20vcGhvdG8uanBn.jpg
```

Progressive JPEG (`jpgo:1:...`) with full-resolution 4:4:4 chroma
(`jpgo:...:1`, instead of this service's default 4:2:2), quality 85. Only
the first two of imgproxy's six `jpgo:` slots exist here — see
[JPEG tuning](api-reference.md#jpeg-tuning-jpgo) in the API reference.

### Lossless WebP

```
GET /rsHnTgOJn7x03v3BhpexkmKoykNr8l-ivAMKc1GYUOk/webpo:lossless/aHR0cHM6Ly9pbWFnZXMuZXhhbXBsZS5jb20vcGhvdG8uanBn.webp
```

### Combining options

Processing options can be freely combined as additional `/`-delimited
segments, in any order:

```
GET /vtDmKFzheWdTv2Q8Kv2BVx0o77KVyvif0Z2nfxBlSEQ/rs:fill:800:600/q:80/bl:5/g:true/el:1/aHR0cHM6Ly9leGFtcGxlLmNvbS9pbWFnZS5qcGc.webp
```

Fill-crops to 800x600, quality 80, blur sigma 5, grayscale, enlarge
permitted, output WebP.

### Using a plain (non-base64) source URL

The `plain/` form skips base64 encoding, at the cost of needing the URL's
own reserved characters percent-encoded:

```
GET /{signature}/rs:fill:800:600/plain/https%3A%2F%2Fexample.com%2Fimage.jpg.webp
```

## Client integration examples

### cURL

```bash
KEY_HEX="6d792d7369676e696e672d6b6579"
SALT_HEX="6d792d73616c74"
SOURCE=$(python3 -c "import base64;print(base64.urlsafe_b64encode(b'https://example.com/image.jpg').rstrip(b'=').decode())")
PATH_TO_SIGN="/rs:fill:800:600/${SOURCE}.jpg"
SIGNATURE=$(python3 -c "
import hashlib, hmac, base64
key = bytes.fromhex('$KEY_HEX')
salt = bytes.fromhex('$SALT_HEX')
mac = hmac.new(key, salt + '$PATH_TO_SIGN'.encode(), hashlib.sha256).digest()
print(base64.urlsafe_b64encode(mac).rstrip(b'=').decode())
")

curl -L -o resized.jpg "https://your-service.com/${SIGNATURE}${PATH_TO_SIGN}"
```

### HTML

```html
<img src="https://your-service.com/{signature}/rs:fill:800:600/{base64 source}.jpg" alt="Resized Image">
```

### JavaScript

```javascript
async function signedImageUrl(baseUrl, keyHex, saltHex, sourceUrl, options) {
  const enc = new TextEncoder();
  const key = await crypto.subtle.importKey(
    "raw",
    hexToBytes(keyHex),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign"],
  );

  const encodedSource = base64UrlEncode(enc.encode(sourceUrl));
  const path = `/${options}/${encodedSource}.jpg`;
  const salt = hexToBytes(saltHex);
  const payload = new Uint8Array([...salt, ...enc.encode(path)]);
  const mac = await crypto.subtle.sign("HMAC", key, payload);
  const signature = base64UrlEncode(new Uint8Array(mac));

  return `${baseUrl}/${signature}${path}`;
}

function hexToBytes(hex) {
  return new Uint8Array(hex.match(/.{2}/g).map((b) => parseInt(b, 16)));
}

function base64UrlEncode(bytes) {
  return btoa(String.fromCharCode(...bytes))
    .replace(/\+/g, "-")
    .replace(/\//g, "_")
    .replace(/=+$/, "");
}
```

### Python

```python
import hashlib
import hmac
import base64
import requests

def signed_image_url(base_url, key_hex, salt_hex, source_url, options):
    key = bytes.fromhex(key_hex)
    salt = bytes.fromhex(salt_hex)
    encoded_source = base64.urlsafe_b64encode(source_url.encode()).rstrip(b"=").decode()
    path = f"/{options}/{encoded_source}.jpg"
    mac = hmac.new(key, salt + path.encode(), hashlib.sha256).digest()
    signature = base64.urlsafe_b64encode(mac).rstrip(b"=").decode()
    return f"{base_url}/{signature}{path}"

url = signed_image_url(
    "https://your-service.com",
    "6d792d7369676e696e672d6b6579",
    "6d792d73616c74",
    "https://example.com/image.jpg",
    "rs:fill:800:600/q:80",
)

response = requests.get(url, allow_redirects=True)
with open("resized.jpg", "wb") as f:
    f.write(response.content)
```
