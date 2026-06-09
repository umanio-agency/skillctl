# Third-party notices

skillctl includes work derived from the following third-party projects.

## agent-skill-manager (asm)

The content-audit signature taxonomy in `src/audit.rs` — the categories
(embedded credentials, obfuscation, shell execution, dynamic code) and several
detection patterns — is adapted from the security auditor of
[`luongnv89/asm`](https://github.com/luongnv89/asm) (`src/security-auditor.ts`).
The signatures were transposed to Rust and re-tuned for markdown skill content
(JS/TS-specific API detectors dropped; a prompt-injection category added); no
source code was copied verbatim.

asm is distributed under the MIT License:

```
MIT License

Copyright (c) 2025 luongnv89

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
