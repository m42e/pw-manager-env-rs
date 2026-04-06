# Self-Hosting

This skill assumes the common split between three services:

- Excalidraw web app.
- Excalidraw collaboration room.
- Excalidraw store service.

No separate orchestration server is part of this stack.

## Web App URL Wiring

Common environment variables used by self-hosted web deployments:

- `VITE_APP_WS_SERVER_URL`: collaboration room URL.
- `VITE_APP_BACKEND_STORE`: store base URL.
- `VITE_APP_BACKEND_V2_GET_URL`: v2 GET base URL.
- `VITE_APP_BACKEND_V2_POST_URL`: v2 POST URL.

Some web images hardcode the upstream collaboration and store URLs in built assets. In that case, deployments often patch the generated JavaScript files at container start.

## Store Service

Common store-side variables:

- `EXCALIDRAW_STORE_BASE_URL`: public base URL for the store.
- `EXCALIDRAW_STORE_DATA_DIR`: persistent data location.

Common health endpoint:

- `/healthz`

## Separation Of Concerns

- The browser URL is where users open Excalidraw.
- The room URL is where collaboration WebSocket traffic goes.
- The store URL is where scenes are persisted and fetched.

Do not assume those three URLs are the same host.

## Script Mapping

The bundled helper scripts map to those services like this:

- `publish-scene-link.mjs`: POSTs to the v2 store endpoint.
- `fetch-scene.mjs`: GETs from the v2 store endpoint.
- `check-api-health.mjs`: checks the store health endpoint.

## Operational Notes

- Keep the browser base URL separate from the POST endpoint when constructing share links.
- Use explicit environment variables instead of hardcoding deployment-specific domains into the skill.
- If a deployment ships a patched web image, document that patching step beside the deployment config rather than embedding it into scene-authoring instructions.