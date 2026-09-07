# pdf2md-wasm

> **High-Performance In-Browser PDF-to-Markdown Engine**  
> 100% Client-Side Privacy • $0.00 Cloud Infrastructure Cost • Sub-Millisecond Rust Core

`pdf2md-wasm` compiles the native `pdf2md-core` engine directly to WebAssembly (`wasm32-unknown-unknown`) using `wasm-bindgen`. It allows web apps, Obsidian plugins, and local tools to parse PDFs into clean GitHub Flavored Markdown (GFM) tables directly on the user's device without transmitting any data over the network.

## Build Instructions

```bash
# Install wasm-pack if needed
cargo install wasm-pack

# Build for modern web browsers (ES Modules)
wasm-pack build --target web --out-dir pkg --release
```

## JavaScript / TypeScript Usage

```javascript
import init, { convert_pdf, is_digital_pdf } from './pkg/pdf2md_wasm.js';

async function run() {
  await init();

  const response = await fetch('document.pdf');
  const buffer = new Uint8Array(await response.arrayBuffer());

  if (is_digital_pdf(buffer)) {
    const result = convert_pdf(buffer, true);
    console.log("Markdown Output:", result.markdown);
    console.log(`Converted in ${result.duration_us / 1000} ms`);
  }
}
```

## License

Licensed under the **[Business Source License 1.1 (BSL-1.1)](../../LICENSE)**. Free for client-side applications, web sandboxes, and Obsidian plugins; commercial SaaS hosting prohibited. Converts to Apache-2.0 / MIT on Sept 1, 2029.
