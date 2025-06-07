# Usage Examples

This page provides practical examples of how to use the Image Resize Service API.

## Basic Resizing

### Resize to Specific Dimensions

```
GET /api/v1/resize?url=https://example.com/image.jpg&width=800&height=600
```

This will resize the image to 800x600 pixels, maintaining the aspect ratio.

### Resize to Specific Width (Maintaining Aspect Ratio)

```
GET /api/v1/resize?url=https://example.com/image.jpg&width=800
```

This will resize the image to 800 pixels wide, maintaining the aspect ratio.

### Resize to Specific Height (Maintaining Aspect Ratio)

```
GET /api/v1/resize?url=https://example.com/image.jpg&height=600
```

This will resize the image to 600 pixels tall, maintaining the aspect ratio.

## Format Conversion

### Convert to WebP

```
GET /api/v1/resize?url=https://example.com/image.jpg&format=webp
```

This will convert the image to WebP format without resizing.

### Convert to JPEG with Quality Setting

```
GET /api/v1/resize?url=https://example.com/image.png&format=jpeg&quality=85
```

This will convert the image to JPEG format with 85% quality.

## Fit Methods

### Cover (Crop to Fill)

```
GET /api/v1/resize?url=https://example.com/image.jpg&width=800&height=600&fit=cover
```

This will resize the image to 800x600 pixels, cropping if necessary to maintain the aspect ratio.

### Contain (Letterbox)

```
GET /api/v1/resize?url=https://example.com/image.jpg&width=800&height=600&fit=contain
```

This will resize the image to fit within 800x600 pixels, adding letterboxing if necessary.

### Fill (Stretch)

```
GET /api/v1/resize?url=https://example.com/image.jpg&width=800&height=600&fit=fill
```

This will stretch the image to 800x600 pixels, potentially distorting the aspect ratio.

## Client Integration Examples

### HTML

```html
<img src="https://your-service.com/api/v1/resize?url=https://example.com/image.jpg&width=800" alt="Resized Image">
```

### JavaScript Fetch

```javascript
fetch('https://your-service.com/api/v1/resize?url=https://example.com/image.jpg&width=800')
  .then(response => response.blob())
  .then(blob => {
    const img = document.createElement('img');
    img.src = URL.createObjectURL(blob);
    document.body.appendChild(img);
  });
```

### cURL

```bash
curl -o resized.jpg "https://your-service.com/api/v1/resize?url=https://example.com/image.jpg&width=800"
```

### Python Requests

```python
import requests
from PIL import Image
from io import BytesIO

response = requests.get(
    "https://your-service.com/api/v1/resize",
    params={
        "url": "https://example.com/image.jpg",
        "width": 800,
        "format": "webp"
    }
)

img = Image.open(BytesIO(response.content))
img.save("resized.webp")