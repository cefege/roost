# Third-party notices

Parts of the agent harness (`crates/roost-llm`, `crates/roost-agent`,
`crates/roost-agent-tools`, and the agent-chat tool shapes in
`crates/roost-protocol`) are ported from the projects below. Each ported file
names its source path on the first line of its `//!` header.

## oh-my-pi

Source: https://github.com/can1357/oh-my-pi at commit
`d46bd42f39e82d41b4a04c022fb94e595bd8c3c8`.

```
MIT License

Copyright (c) 2025 Mario Zechner
Copyright (c) 2025-2026 Can Bölük
Copyright (c) 2026 Stencil Labs, Inc.

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

## pi-ai (`@earendil-works/pi-ai` 1.1.0)

Source: https://github.com/earendil-works/pi, package `@earendil-works/pi-ai`
version 1.1.0, author Mario Zechner, license MIT (as declared in the
package's `package.json`). The model catalog JSON in
`crates/roost-llm/catalog/` and the provider request, streaming and OAuth
logic are ported from its compiled `dist/` sources. The MIT license text above
applies with the copyright line `Copyright (c) 2025 Mario Zechner`.
