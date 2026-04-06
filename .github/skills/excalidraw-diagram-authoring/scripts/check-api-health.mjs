#!/usr/bin/env node

function parseArgs(argv) {
  const options = {};

  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];

    if (arg === "--url") {
      options.url = argv[++index];
    } else if (arg === "--base-url") {
      options.baseUrl = argv[++index];
    } else if (arg === "--path") {
      options.path = argv[++index];
    } else if (arg === "--json") {
      options.json = true;
    } else if (arg === "--help" || arg === "-h") {
      options.help = true;
    } else {
      throw new Error(`Unknown argument: ${arg}`);
    }
  }

  return options;
}

function usage() {
  return [
    "Usage:",
    "  node check-api-health.mjs --base-url https://excalidraw-service.pb42.de [--path /healthz] [--json]",
    "  node check-api-health.mjs --url https://excalidraw-service.pb42.de/healthz",
    "",
    "Environment fallbacks:",
    "  EXCALIDRAW_STORE_HEALTH_URL",
    "  EXCALIDRAW_STORE_BASE_URL",
    "  VITE_APP_BACKEND_STORE",
  ].join("\n");
}

function normalizeUrl(options) {
  if (options.url) {
    return options.url;
  }

  const baseUrl =
    options.baseUrl ||
    process.env.EXCALIDRAW_STORE_BASE_URL ||
    process.env.VITE_APP_BACKEND_STORE;
  const healthUrl = process.env.EXCALIDRAW_STORE_HEALTH_URL;

  if (!baseUrl && healthUrl) {
    return healthUrl;
  }

  if (!baseUrl) {
    throw new Error("Provide --url, --base-url, or EXCALIDRAW_STORE_HEALTH_URL.");
  }

  const path = options.path || "/healthz";
  return new URL(path, baseUrl.endsWith("/") ? baseUrl : `${baseUrl}/`).toString();
}

async function main() {
  const options = parseArgs(process.argv.slice(2));

  if (options.help) {
    process.stdout.write(`${usage()}\n`);
    return;
  }

  const url = normalizeUrl(options);
  const response = await fetch(url, { method: "GET" });
  const result = { ok: response.ok, status: response.status, url };

  if (options.json) {
    process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
  } else {
    process.stdout.write(`${response.ok ? "OK" : "FAIL"} ${response.status} ${url}\n`);
  }

  if (!response.ok) {
    process.exitCode = 1;
  }
}

main().catch((error) => {
  process.stderr.write(`${error.message}\n\n${usage()}\n`);
  process.exitCode = 1;
});