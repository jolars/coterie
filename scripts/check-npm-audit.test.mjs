import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { test } from "node:test";

const checker = new URL("./check-npm-audit.mjs", import.meta.url);

function check(advisories) {
  const report = {
    advisories: Object.fromEntries(advisories.map((advisory, index) => [index, advisory])),
    metadata: {
      vulnerabilities: { moderate: advisories.length },
    },
  };
  return spawnSync(process.execPath, [checker.pathname], {
    input: JSON.stringify(report),
    encoding: "utf8",
  });
}

function advisory(id, name, path) {
  return {
    github_advisory_id: id,
    module_name: name,
    findings: [{ paths: [path] }],
  };
}

test("accepts the four tracked VitePress advisories", () => {
  const result = check([
    advisory("GHSA-67mh-4wv8-2f99", "esbuild", ".>vitepress>vite>esbuild"),
    advisory("GHSA-4w7w-66w2-5vf9", "vite", ".>vitepress>vite"),
    advisory("GHSA-v6wh-96g9-6wx3", "vite", ".>vitepress>vite"),
    advisory("GHSA-fx2h-pf6j-xcff", "vite", ".>vitepress>vite"),
  ]);
  assert.equal(result.status, 0);
  assert.match(result.stdout, /4 tracked VitePress advisories/);
});

test("rejects a new advisory", () => {
  const result = check([advisory("GHSA-xxxx-yyyy-zzzz", "vite", ".>vitepress>vite")]);
  assert.equal(result.status, 1);
  assert.match(result.stderr, /GHSA-xxxx-yyyy-zzzz/);
});

test("rejects a tracked advisory on another dependency path", () => {
  const result = check([advisory("GHSA-fx2h-pf6j-xcff", "vite", ".>another-package>vite")]);
  assert.equal(result.status, 1);
});

test("rejects a malformed audit response", () => {
  const result = spawnSync(process.execPath, [checker.pathname], {
    input: '{"error":"registry unavailable"}',
    encoding: "utf8",
  });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /Could not check the npm audit report/);
});
