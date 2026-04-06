import { copyFile, mkdir } from "node:fs/promises";
import { resolve } from "node:path";

const root = resolve(import.meta.dirname, "..");
const publicDir = resolve(root, "docs", "public");
const installSource = resolve(root, "scripts", "install.sh");
const installDestination = resolve(publicDir, "install.sh");
const imageSourceDir = resolve(root, "docs", "assets", "images");
const imageDestinationDir = resolve(publicDir, "assets", "images");
const imageFiles = ["Logo-pw-env@3x.png"];

await mkdir(publicDir, { recursive: true });
await mkdir(imageDestinationDir, { recursive: true });

await copyFile(installSource, installDestination);

for (const imageFile of imageFiles) {
	await copyFile(resolve(imageSourceDir, imageFile), resolve(imageDestinationDir, imageFile));
}
