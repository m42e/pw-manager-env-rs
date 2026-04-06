#!/usr/bin/env node

import { readFile } from "node:fs/promises";
import { webcrypto } from "node:crypto";
import { deflateSync } from "node:zlib";

function parseArgs(argv) {
  const options = {};

  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];

    if (arg === "--input") {
      options.input = argv[++index];
    } else if (arg === "--stdin") {
      options.stdin = true;
    } else if (arg === "--scene-json") {
      options.sceneJson = argv[++index];
    } else if (arg === "--post-url") {
      options.postUrl = argv[++index];
    } else if (arg === "--app-url") {
      options.appUrl = argv[++index];
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
    "  node publish-scene-link.mjs --input diagram.excalidraw [--post-url url] [--app-url url] [--json]",
    "  node publish-scene-link.mjs --stdin [--post-url url] [--app-url url]",
    "  node publish-scene-link.mjs --scene-json '{\"type\":\"excalidraw\"...}'",
    "",
    "Environment fallbacks:",
    "  EXCALIDRAW_POST_URL or VITE_APP_BACKEND_V2_POST_URL",
    "  EXCALIDRAW_BROWSER_URL",
  ].join("\n");
}

function concatBuffers(...buffers) {
  let total = 4;

  for (const buffer of buffers) {
    total += 4 + buffer.length;
  }

  const output = new Uint8Array(total);
  const view = new DataView(output.buffer);
  view.setUint32(0, 1);

  let offset = 4;

  for (const buffer of buffers) {
    view.setUint32(offset, buffer.length);
    offset += 4;
    output.set(buffer, offset);
    offset += buffer.length;
  }

  return output;
}

async function readStdin() {
  const chunks = [];

  for await (const chunk of process.stdin) {
    chunks.push(typeof chunk === "string" ? Buffer.from(chunk) : chunk);
  }

  return Buffer.concat(chunks).toString("utf8");
}

async function loadSceneJson(options) {
  if (options.sceneJson) {
    return options.sceneJson;
  }

  if (options.input) {
    return readFile(options.input, "utf8");
  }

  if (options.stdin || !process.stdin.isTTY) {
    return readStdin();
  }

  throw new Error("Provide --input, --scene-json, or --stdin.");
}

function normalizeBaseUrl(url) {
  return url.endsWith("/") ? url.slice(0, -1) : url;
}

async function main() {
  const options = parseArgs(process.argv.slice(2));

  if (options.help) {
    process.stdout.write(`${usage()}\n`);
    return;
  }

  const postUrl =
    options.postUrl ||
    process.env.EXCALIDRAW_POST_URL ||
    process.env.VITE_APP_BACKEND_V2_POST_URL ||
    "https://excalidraw-service.pb42.de/api/v2/post/";
  const appUrl = normalizeBaseUrl(
    options.appUrl || process.env.EXCALIDRAW_BROWSER_URL || "https://excalidraw.pb42.de",
  );
  const sceneJson = await loadSceneJson(options);

  JSON.parse(sceneJson);

  const textEncoder = new TextEncoder();
  const metadata = textEncoder.encode(JSON.stringify({}));
  const data = textEncoder.encode(sceneJson);
  const innerPayload = concatBuffers(metadata, data);
  const compressed = deflateSync(Buffer.from(innerPayload));
  const subtle = webcrypto.subtle;
  const aesKey = await subtle.generateKey(
    { name: "AES-GCM", length: 128 },
    true,
    ["encrypt"],
  );
  const iv = webcrypto.getRandomValues(new Uint8Array(12));
  const encrypted = new Uint8Array(
    await subtle.encrypt({ name: "AES-GCM", iv }, aesKey, compressed),
  );
  const encoding = textEncoder.encode(
    JSON.stringify({ version: 2, compression: "pako@1", encryption: "AES-GCM" }),
  );
  const outerPayload = Buffer.from(concatBuffers(encoding, iv, encrypted));

  const response = await fetch(postUrl, {
    method: "POST",
    body: outerPayload,
  });

  if (!response.ok) {
    throw new Error(`Upload failed: ${response.status} ${response.statusText}`);
  }

  const body = await response.json();
  const jwk = await subtle.exportKey("jwk", aesKey);

  if (typeof body?.id !== "string" || typeof jwk?.k !== "string") {
    throw new Error("Store response did not include an id or exportable key.");
  }

  const url = `${appUrl}/#json=${body.id},${jwk.k}`;

  if (options.json) {
    process.stdout.write(
      `${JSON.stringify({ id: body.id, url, postUrl }, null, 2)}\n`,
    );
    return;
  }

  process.stdout.write(`${url}\n`);
}

main().catch((error) => {
  process.stderr.write(`${error.message}\n\n${usage()}\n`);
  process.exitCode = 1;
});