#!/usr/bin/env node

import { readFile, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

function parseArgs(argv) {
  const options = {};

  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];

    if (arg === "--output") {
      options.output = argv[++index];
    } else if (arg === "--source") {
      options.source = argv[++index];
    } else if (arg === "--background") {
      options.background = argv[++index];
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
    "  node new-scene.mjs [--output file] [--source url] [--background #ffffff]",
    "",
    "Examples:",
    "  node new-scene.mjs --output diagram.excalidraw",
    "  node new-scene.mjs --source https://example.com --background '#f8fafc'",
  ].join("\n");
}

async function main() {
  const options = parseArgs(process.argv.slice(2));

  if (options.help) {
    process.stdout.write(`${usage()}\n`);
    return;
  }

  const scriptDir = path.dirname(fileURLToPath(import.meta.url));
  const templatePath = path.join(scriptDir, "..", "assets", "scene-template.json");
  const template = JSON.parse(await readFile(templatePath, "utf8"));

  if (options.source) {
    template.source = options.source;
  }

  if (options.background) {
    template.appState.viewBackgroundColor = options.background;
  }

  const output = `${JSON.stringify(template, null, 2)}\n`;

  if (options.output) {
    await writeFile(options.output, output, "utf8");
    process.stdout.write(`Wrote ${options.output}\n`);
    return;
  }

  process.stdout.write(output);
}

main().catch((error) => {
  process.stderr.write(`${error.message}\n\n${usage()}\n`);
  process.exitCode = 1;
});