# Documentation site deployment

The public site source is in `website/`. `task docs:site` installs its pinned pnpm dependencies, copies the generated JSON Schemas into the build input, and runs the VitePress production build with broken-link checking. `pnpm docs:dev` binds to `127.0.0.1`. Do not expose the development server to a network while the current Vite alerts remain open. `task check` includes the site build and the npm dependency audit. The existing `task docs` remains the rustdoc gate.

The Pages workflow builds automatically from the tag of a **published GitHub
release** and sets `COTERIE_DOCS_REF` to that tag. Public links to repository
evidence and examples then point to the same source version as the deployed
pages. For documentation updates between releases, manually dispatch the Docs
workflow on `main`; that build links to the current `main` sources. A main-branch
push alone does not deploy the site. v0.2.0 predates the site.

## One-time GitHub and DNS setup

1. In `jolars/coterie` repository Settings → Pages, select **GitHub Actions** as the source and set the custom domain to `coterie.fyi`.
2. At the domain registrar, point the apex `coterie.fyi` to GitHub Pages using the current `A` records or an `ALIAS`/`ANAME`. Add a `www` CNAME pointing directly to `jolars.github.io` so `www.coterie.fyi` redirects to the apex. If Cloudflare manages DNS, set both records to **DNS only** while GitHub provisions its certificate; its Pages health check marks proxied records as ineligible for GitHub HTTPS. Follow [GitHub's current DNS instructions](https://docs.github.com/en/pages/configuring-a-custom-domain-for-your-github-pages-site/managing-a-custom-domain-for-your-github-pages-site).
3. After DNS and the certificate are ready, enable **Enforce HTTPS** in Pages settings. Verify both hostnames and the redirect. The Actions publishing source ignores a repository `CNAME` file, so this site does not need one.

Use the `github-pages` deployment environment for release tags. If an environment protection rule is added, allow release tags to deploy. Re-running a published release's Docs workflow rebuilds the same tag. A new release replaces the site with documentation from its own tag.

## Release smoke check

After the first deployment, confirm `https://coterie.fyi`, `/guide/getting-started`, `/reference/cli`, and `/schemas/config-global-v1.schema.json` load over HTTPS. Check local search, the mobile navigation, edit links, and links to repository files. Confirm the reference matches the newly published binary's `--help` and `config schema` output.
