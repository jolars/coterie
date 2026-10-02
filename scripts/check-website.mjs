import { readFileSync, readdirSync } from "node:fs";

const contract = readFileSync(new URL("../docs/cli-contract.md", import.meta.url), "utf8");
const reference = readFileSync(new URL("../website/reference/cli.md", import.meta.url), "utf8");
const commands = [...contract.matchAll(/^### `(coterie(?: [^`]+)?)`$/gm)]
  .map((match) => match[1]);
const missing = commands.filter((command) => !reference.includes(`\`${command}\``));

if (missing.length > 0) {
  throw new Error(`CLI reference is missing: ${missing.join(", ")}`);
}

const schemaDirectory = new URL("../schemas/", import.meta.url);
const builtSchemas = new URL("../website/.vitepress/dist/schemas/", import.meta.url);
for (const name of readdirSync(schemaDirectory).filter((entry) => entry.endsWith(".json"))) {
  const source = readFileSync(new URL(name, schemaDirectory));
  const published = readFileSync(new URL(name, builtSchemas));
  if (!source.equals(published)) {
    throw new Error(`Published schema differs from generated source: ${name}`);
  }
}
