// Stable VitePress still requires Vite 5; TODO.md tracks removal after its upgrade.
const tracked = new Map([
  ["GHSA-67mh-4wv8-2f99", ["esbuild", ".>vitepress>vite>esbuild"]],
  ["GHSA-4w7w-66w2-5vf9", ["vite", ".>vitepress>vite"]],
  ["GHSA-v6wh-96g9-6wx3", ["vite", ".>vitepress>vite"]],
  ["GHSA-fx2h-pf6j-xcff", ["vite", ".>vitepress>vite"]],
]);

let input = "";
for await (const chunk of process.stdin) input += chunk;

try {
  const report = JSON.parse(input);
  const advisories = Object.values(report.advisories);
  const counts = Object.values(report.metadata.vulnerabilities);
  if (
    counts.some((count) => !Number.isInteger(count) || count < 0) ||
    counts.reduce((sum, count) => sum + count, 0) !== advisories.length
  ) {
    throw new Error("The advisory list does not match the audit totals.");
  }

  const unexpected = advisories.filter((advisory) => {
    const expected = tracked.get(advisory.github_advisory_id);
    return (
      !expected ||
      advisory.module_name !== expected[0] ||
      !Array.isArray(advisory.findings) ||
      advisory.findings.length === 0 ||
      advisory.findings.some(
        (finding) =>
          !Array.isArray(finding.paths) ||
          finding.paths.length === 0 ||
          finding.paths.some((path) => path !== expected[1]),
      )
    );
  });

  if (unexpected.length > 0) {
    for (const advisory of unexpected) {
      console.error(
        `Untracked npm advisory: ${advisory.github_advisory_id ?? "unknown"} (${advisory.module_name ?? "unknown package"}).`,
      );
    }
    process.exitCode = 1;
  } else {
    console.log(
      `npm audit: ${advisories.length} tracked VitePress advisories; no new advisories. Run pnpm audit for details.`,
    );
  }
} catch (error) {
  console.error(`Could not check the npm audit report: ${error.message}`);
  process.exitCode = 1;
}
