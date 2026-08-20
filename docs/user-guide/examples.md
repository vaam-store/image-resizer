# Usage Examples

Practical examples against the real, signed-URL API (see the
[API reference](api-reference.md) for the full grammar). All examples below
use the same placeholder key/salt as the API reference's worked example -
`SIGNING_KEY=6d792d7369676e696e672d6b6579`,
`SIGNING_SALT=6d792d73616c74` (hex for `my-signing-key` / `my-salt`) -
never use these for anything real.

!!! note "The resize endpoint redirects, it doesn't stream bytes back"
    `GET /{signature}/{options}/{source}.{extension}` responds
    `301 Moved Permanently` with a `Location` header pointing at the
    resized image, rather than returning the image bytes directly.
    Browsers, `fetch()`, and `curl -L` all follow that redirect
    transparently, so most of the examples below work unmodified - just be
    aware a plain `curl` (no `-L`) will save the (empty) redirect
    response, not an image.

!!! note "Every example needs a real signature"
    A request with a missing, wrong, or (unless `ALLOW_UNSIGNED_REQUESTS=true`
    is set) `unsigned` signature gets `403 Forbidden`, never processed.
    The signature is `base64url_nopad(HMAC-SHA256(key, salt || path))`
    where `path` is everything in the URL *after* the signature segment
    (leading `/` included). The Python snippet under
    [Computing a signature](#computing-a-signature) below is the easiest
    way to generate one for these examples.

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

source = encode_source("https://example.com/image.jpg")
path = f"/rs:fill:800:600/{source}.jpg"
signature = sign(key_hex, salt_hex, path)

print(f"/{signature}{path}")
```

## Basic resizing

### Resize to specific dimensions

```
GET /{signature}/rs:fill:800:600/{base64 source}.jpg
```

Resizes and crops to fill exactly 800x600.

### Resize to a specific width only

```
GET /{signature}/rs::800:0/{base64 source}.jpg
```

`0` means "not set" for either dimension - only width is applied, height
preserves aspect ratio.

### Resize to a specific height only

```
GET /{signature}/rs::0:600/{base64 source}.jpg
```

## Format conversion

### Convert to WebP

```
GET /{signature}/{base64 source}.webp
```

The trailing extension is always the output format, independent of the
source's own extension or content type.

### Convert to JPEG

```
GET /{signature}/{base64 source}.jpg
```

## Quality, blur, and grayscale

### Encode quality

```
GET /{signature}/q:80/{base64 source}.jpg
```

### Grayscale

```
GET /{signature}/rs:fill:800:600/g:true/{base64 source}.jpg
```

### Gaussian blur

```
GET /{signature}/rs:fill:800:600/bl:8/{base64 source}.jpg
```

### Combining options

Processing options can be freely combined as additional `/`-delimited
segments, in any order:

```
GET /{signature}/rs:fill:800:600/q:80/bl:5/g:true/el:1/{base64 source}.webp
```

## Using a plain (non-base64) source URL

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
