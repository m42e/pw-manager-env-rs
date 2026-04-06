#!/usr/bin/env node

import { writeFile } from "node:fs/promises";
import { webcrypto } from "node:crypto";
import { inflateSync } from "node:zlib";

function parseArgs(argv) {
  const options = {};

  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];

    if (arg === "--url") {
      options.url = argv[++index];
    } else if (arg === "--id") {
      options.id = argv[++index];
    } else if (arg === "--key") {
      options.key = argv[++index];
    } else if (arg === "--get-url-base") {
      options.getUrlBase = argv[++index];
    } else if (arg === "--output") {
      options.output = argv[++index];
    } else if (arg === "--pretty") {
      options.pretty = true;
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
    "  node fetch-scene.mjs --url 'https://excalidraw.pb42.de/#json=<id>,<key>' [--output file] [--pretty]",
    "  node fetch-scene.mjs --id <id> --key <key> [--get-url-base url]",
    "",
    "Environment fallbacks:",
    "  EXCALIDRAW_GET_URL_BASE or VITE_APP_BACKEND_V2_GET_URL",
  ].join("\n");
}

function parseShareReference(options) {
  if (options.url) {
    const url = new URL(options.url);
    const fragment = url.hash.startsWith("#") ? url.hash.slice(1) : url.hash;

    if (!fragment.startsWith("json=")) {
      throw new Error("Share URL does not contain a #json=<id>,<key> fragment.");
    }

    const [id, key] = fragment.slice(5).split(",");

    if (!id || !key) {
      throw new Error("Share URL fragment is missing an id or key.");
    }

    return { id, key };
  }

  if (options.id && options.key) {
    return { id: options.id, key: options.key };
  }

  throw new Error("Provide --url or both --id and --key.");
}

function normalizeGetBase(url) {
  return url.endsWith("/") ? url : `${url}/`;
}

function splitConcatBuffer(bytes) {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);

  if (bytes.byteLength < 4) {
    throw new Error("Concat buffer is too small.");
  }

  const version = view.getUint32(0);

  if (version !== 1) {
    throw new Error(`Unsupported concat buffer version: ${version}`);
  }

  const parts = [];
  let offset = 4;

  while (offset < bytes.byteLength) {
    if (offset + 4 > bytes.byteLength) {
      throw new Error("Concat buffer length header is truncated.");
    }

    const length = view.getUint32(offset);
    offset += 4;

    if (offset + length > bytes.byteLength) {
      throw new Error("Concat buffer part exceeds payload size.");
    }

    parts.push(bytes.subarray(offset, offset + length));
    offset += length;
  }

  return parts;
}

async function main() {
  const options = parseArgs(process.argv.slice(2));

  if (options.help) {
    process.stdout.write(`${usage()}\n`);
    return;
  }

  const { id, key } = parseShareReference(options);
  const getUrlBase = normalizeGetBase(
    options.getUrlBase ||
      process.env.EXCALIDRAW_GET_URL_BASE ||
      process.env.VITE_APP_BACKEND_V2_GET_URL ||
      "https://excalidraw-service.pb42.de/api/v2/",
  );
  const response = await fetch(`${getUrlBase}${id}`);

  if (!response.ok) {
    throw new Error(`Fetch failed: ${response.status} ${response.statusText}`);
  }

  const outerBytes = new Uint8Array(await response.arrayBuffer());
  const [encodingBytes, iv, encrypted] = splitConcatBuffer(outerBytes);
  const encoding = JSON.parse(new TextDecoder().decode(encodingBytes));

  if (encoding.encryption !== "AES-GCM") {
    throw new Error(`Unsupported encryption: ${encoding.encryption}`);
  }

  const subtle = webcrypto.subtle;
  const cryptoKey = await subtle.importKey(
    "jwk",
    { kty: "oct", alg: "A128GCM", ext: true, k: key },
    { name: "AES-GCM" },
    false,
    ["decrypt"],
  );
  const decrypted = new Uint8Array(
    await subtle.decrypt({ name: "AES-GCM", iv }, cryptoKey, encrypted),
  );
  const inflated = inflateSync(Buffer.from(decrypted));
  const [, sceneBytes] = splitConcatBuffer(new Uint8Array(inflated));
  let output = new TextDecoder().decode(sceneBytes);

  if (options.pretty) {
    output = `${JSON.stringify(JSON.parse(output), null, 2)}\n`;
  }

  if (options.output) {
    await writeFile(options.output, output, "utf8");
    process.stdout.write(`Wrote ${options.output}\n`);
    return;
  }

  process.stdout.write(output.endsWith("\n") ? output : `${output}\n`);
}

main().catch((error) => {
  process.stderr.write(`${error.message}\n\n${usage()}\n`);
  process.exitCode = 1;
});