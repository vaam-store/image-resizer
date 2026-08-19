# Usage Examples

Practical examples against the real API (see the
[API reference](api-reference.md) for the full parameter list - `quality`
and `fit` do not exist; the equivalents that do are noted below).

!!! note "The resize endpoint redirects, it doesn't stream bytes back"
    `GET /api/images/resize` responds `301 Moved Permanently` with a
    `Location` header pointing at the resized image, rather than
    returning the image bytes directly. Browsers, `fetch()`, and
    `curl -L` all follow that redirect transparently, so most of the
    examples below work unmodified - just be aware a plain `curl` (no
    `-L`) will save the (empty) redirect response, not an image.

## Basic resizing

### Resize to specific dimensions

```
GET /api/images/resize?url=https://example.com/image.jpg&width=800&height=600&format=jpg
```

Resizes the image to fit within 800x600 pixels, maintaining the aspect
ratio (pass `enlarge`-related behavior aside, output never exceeds the
source's own aspect-preserving fit into the requested box).

### Resize to a specific width only

```
GET /api/images/resize?url=https://example.com/image.jpg&width=800&format=jpg
```

### Resize to a specific height only

```
GET /api/images/resize?url=https://example.com/image.jpg&height=600&format=jpg
```

## Format conversion

### Convert to WebP

```
GET /api/images/resize?url=https://example.com/image.jpg&format=webp
```

### Convert to JPEG

```
GET /api/images/resize?url=https://example.com/image.png&format=jpg
```

There is no `quality` parameter.

## Blur and grayscale

There is no `fit` parameter (no `cover`/`contain`/`fill` modes) - the two
transform parameters beyond width/height/format are `blur_sigma` and
`grayscale`.

### Grayscale

```
GET /api/images/resize?url=https://example.com/image.jpg&width=800&height=600&grayscale=true
```

### Gaussian blur

```
GET /api/images/resize?url=https://example.com/image.jpg&width=800&height=600&blur_sigma=8
```

`blur_sigma` accepts 0-100 (default 5).

## Client integration examples

### HTML

```html
<img src="https://your-service.com/api/images/resize?url=https://example.com/image.jpg&width=800&format=jpg" alt="Resized Image">
```

### JavaScript Fetch

`fetch()` follows redirects by default, so this resolves to the
resized image's bytes:

```javascript
fetch('https://your-service.com/api/images/resize?url=https://example.com/image.jpg&width=800&format=jpg')
  .then(response => response.blob())
  .then(blob => {
    const img = document.createElement('img');
    img.src = URL.createObjectURL(blob);
    document.body.appendChild(img);
  });
```

### cURL

`-L` is required to follow the `301` redirect:

```bash
curl -L -o resized.jpg "https://your-service.com/api/images/resize?url=https://example.com/image.jpg&width=800&format=jpg"
```

### Python Requests

`requests` follows redirects by default:

```python
import requests
from PIL import Image
from io import BytesIO

response = requests.get(
    "https://your-service.com/api/images/resize",
    params={
        "url": "https://example.com/image.jpg",
        "width": 800,
        "format": "webp",
    },
)

img = Image.open(BytesIO(response.content))
img.save("resized.webp")
```
