# Third-party notices

## t3code

<https://github.com/pingdotgg/t3code>

Threadknot's agent layer is a from-scratch reimplementation of t3code's agent
integration, and several pieces are direct ports — each marked in the source:

- `src/components/ContextMeter.tsx` and its styles in `src/styles.css` — ported
  from t3code's `ContextWindowMeter`.
- `src-tauri/src/agents/codex.rs` — the Codex app-server wire integration.
- `src-tauri/src/ports.rs` — the dev-server port-scanning pattern.
- `src-tauri/src/mcp.rs` — the streamable-HTTP MCP endpoint shape.

t3code is distributed under the MIT License:

```
MIT License

Copyright (c) 2026 T3 Tools Inc.

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
