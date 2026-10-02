import { cpSync, mkdirSync, readdirSync, rmSync } from "node:fs";

const schemaDirectory = new URL("../schemas/", import.meta.url);
const outputDirectory = new URL("../website/public/schemas/", import.meta.url);

rmSync(outputDirectory, { recursive: true, force: true });
mkdirSync(outputDirectory, { recursive: true });
for (const name of readdirSync(schemaDirectory)) {
  if (name.endsWith(".json")) {
    cpSync(new URL(name, schemaDirectory), new URL(name, outputDirectory));
  }
}
