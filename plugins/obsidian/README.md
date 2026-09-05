# Obsidian PDF to Markdown Converter (`obsidian-pdf2md`)

> **Sub-Millisecond PDF to Markdown with 2D Table Reconstruction for Obsidian PKM**  
> Local-First Privacy • Zero Cloud Leaks • Pristine GitHub Flavored Markdown Pipe Tables

Convert any PDF document directly into clean, editable Markdown within your Obsidian vault.

## Features

- **Drag-and-Drop Ingestion:** Simply drag any PDF file directly into your active note to convert and inject clean Markdown at your cursor position.
- **Pristine Table Formatting:** Reconstructs complex tables into formatted GitHub-flavored pipe tables (`| Col 1 | Col 2 |`).
- **Local & Self-Hosted First:** Connects directly to your local `markdown-extract-service` container on `http://127.0.0.1:3984` — 100% offline and free.
- **Optional Cloud Gateway:** Supports remote SaaS API keys with Keycloak bearer tokens.
- **Context Menu Action:** Right-click any PDF in the vault file tree to convert directly to `<name>.md`.

## Development & Building

```bash
cd plugins/obsidian
npm install
npm run build
```

Copy `main.js` and `manifest.json` into your vault's `.obsidian/plugins/pdf2md-converter/` directory.

## License

MIT License.
