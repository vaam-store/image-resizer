# EmgR

## Getting Started
1. Generate server and client code from the OpenAPI spec:
   ```bash
   make init
   ```
   
2. Build the project:
   ```bash
   make build
   ```
   
3. Start the project:
   ```bash
   make up-app
   ```

4. Run tests:
   ```bash
   curl -LI http://localhost:13001/api/images/resize?url=https%3A%2F%2Fimages.pexels.com%2Fphotos%2F32138887%2Fpexels-photo-32138887.jpeg%3Fcs%3Dsrgb%26dl%3Dpexels-branka-krnjaja-1475677195-32138887.jpg%26fm%3Djpg%26w%3D1280%26h%3D1910&width=1000&height=1000&format=jpg
   ```

   you'll see
   ```plaintext
   HTTP/1.1 302 Found <<--- This is the interesting code
   location: http://localhost:13001/api/images/files/8da4cab7c984db45f2f6e61e8e0d952332364e65133773472ddf6f823fa1610a.jpg
   vary: origin, access-control-request-method, access-control-request-headers
   access-control-allow-origin: *
   content-length: 0
   date: Sat, 31 May 2025 12:51:52 GMT
   
   HTTP/1.1 200 OK
   content-type: image/png
   vary: origin, access-control-request-method, access-control-request-headers
   access-control-allow-origin: *
   content-length: 105942
   date: Sat, 31 May 2025 12:51:52 GMT
   ```

5. You can open this image in your browser:
   ```bash
   open http://localhost:13001/api/images/files/8da4cab7c984db45f2f6e61e8e0d952332364e65133773472ddf6f823fa1610a.jpg
   ```
   or open the base full one instead:
   ```bash
   open http://localhost:13001/api/images/resize?url=https%3A%2F%2Fimages.pexels.com%2Fphotos%2F32138887%2Fpexels-photo-32138887.jpeg%3Fcs%3Dsrgb%26dl%3Dpexels-branka-krnjaja-1475677195-32138887.jpg%26fm%3Djpg%26w%3D1280%26h%3D1910&width=200&height=200&format=jpg
   ```