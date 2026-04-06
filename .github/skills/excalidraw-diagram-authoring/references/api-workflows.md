# API Workflows

An earlier server-backed workflow used a server tool to publish scenes. This skill replaces that with local scripts.

## Supported Operations

- Publish a scene JSON document to an Excalidraw v2 store endpoint.
- Fetch and decrypt a stored scene back from a share URL.
- Check the store health endpoint.

## Publish A Scene

```bash
node .github/skills/excalidraw-diagram-authoring/scripts/publish-scene-link.mjs \
  --input diagram.excalidraw
```

Useful flags:

- `--input <file>`: read scene JSON from a file.
- `--stdin`: read scene JSON from standard input.
- `--scene-json '<json>'`: pass scene JSON inline.
- `--post-url <url>`: override the v2 POST endpoint.
- `--app-url <url>`: override the browser base used to construct the share URL.
- `--json`: print a structured JSON result instead of only the URL.

Environment fallbacks:

- `EXCALIDRAW_POST_URL`
- `VITE_APP_BACKEND_V2_POST_URL`
- `EXCALIDRAW_BROWSER_URL`

## Fetch A Scene

```bash
node .github/skills/excalidraw-diagram-authoring/scripts/fetch-scene.mjs \
  --url 'https://excalidraw.example.com/#json=<id>,<key>' \
  --pretty \
  --output restored.excalidraw
```

Useful flags:

- `--url <share-url>`: full share URL.
- `--id <id> --key <key>`: explicit identifier and AES key.
- `--get-url-base <url>`: override the v2 GET base URL.
- `--output <file>`: write the scene JSON to a file.
- `--pretty`: pretty-print the JSON result.

Environment fallbacks:

- `EXCALIDRAW_GET_URL_BASE`
- `VITE_APP_BACKEND_V2_GET_URL`

## Health Check

```bash
node .github/skills/excalidraw-diagram-authoring/scripts/check-api-health.mjs \
  --base-url https://store.example.com
```

Defaults:

- Path defaults to `/healthz`.
- `--url` wins over `--base-url` plus `--path`.

Environment fallbacks:

- `EXCALIDRAW_STORE_HEALTH_URL`
- `EXCALIDRAW_STORE_BASE_URL`
- `VITE_APP_BACKEND_STORE`

## Format Notes

The v2 payload format used by the helper scripts is:

1. Inner concat-buffer payload with metadata and scene JSON.
2. zlib deflate compression.
3. AES-GCM encryption.
4. Outer concat-buffer payload with encoding metadata, IV, and ciphertext.

That preserves the share-link format expected by Excalidraw browsers and self-hosted stores.