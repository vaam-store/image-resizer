# License

`emgr` is licensed under the MIT License. The full text below is
reproduced from the repository's [`LICENSE`](https://github.com/vaam-store/image-resizer/blob/main/LICENSE)
file.

```text
MIT License

Copyright (c) 2025 Stephane SEGNING LAMBOU

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## Third-party notices

`emgr` links native code with its own licenses and attribution
obligations, reproduced from the repository's
[`NOTICE`](https://github.com/vaam-store/image-resizer/blob/main/NOTICE)
file:

- **mozjpeg** (via the `mozjpeg`/`mozjpeg-sys` crates, used for DCT-scaled
  and full-size JPEG decoding and for JPEG encoding - see the
  [changelog](changelog.md#performance)) vendors libjpeg-turbo and
  Independent JPEG Group (IJG) code, distributed under the IJG, Zlib and
  BSD-3-Clause licenses. Per the IJG license, this software is based in
  part on the work of the Independent JPEG Group.
- **libwebp** (via the `webp` crate, BSD-3-Clause) is used for lossy WebP
  encoding.

Full license texts for every dependency ship with those crates and are
reproduced in the dependency tree under `~/.cargo/registry` (or your
project's vendored `Cargo.lock`-resolved sources). `cargo-deny`'s
`licenses` check (`deny.toml`, run in CI) enforces that every dependency's
declared license is one this project allows - see
[Contributing](../development/contributing.md) for running it locally.
