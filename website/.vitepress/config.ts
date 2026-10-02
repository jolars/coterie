import { defineConfig } from "vitepress";

const releaseRef = process.env.COTERIE_DOCS_REF;

function pinRepositoryLinks(tokens: any[]): void {
  for (const token of tokens) {
    if (token.type === "link_open") {
      const href = token.attrGet("href");
      const prefix = "https://github.com/jolars/coterie/blob/main/";
      if (href?.startsWith(prefix) && releaseRef) {
        token.attrSet("href", `https://github.com/jolars/coterie/blob/${releaseRef}/${href.slice(prefix.length)}`);
      }
    }
    if (token.children) pinRepositoryLinks(token.children);
  }
}

export default defineConfig({
  title: "Coterie",
  description: "Coordinate coding agents from your project.",
  lang: "en-US",
  base: "/",
  cleanUrls: true,
  lastUpdated: true,
  sitemap: { hostname: "https://coterie.fyi" },
  markdown: {
    config(md) {
      md.core.ruler.push("coterie-release-links", (state) => {
        pinRepositoryLinks(state.tokens);
      });
    },
  },
  head: [["link", { rel: "icon", href: "/favicon.svg" }]],
  themeConfig: {
    nav: [
      { text: "Guide", link: "/guide/getting-started" },
      { text: "Reference", link: "/reference/cli" },
      { text: "GitHub", link: "https://github.com/jolars/coterie" },
    ],
    sidebar: {
      "/guide/": [
        {
          text: "Start here",
          items: [
            { text: "What is Coterie?", link: "/guide/introduction" },
            { text: "Getting started", link: "/guide/getting-started" },
            { text: "Your first run", link: "/guide/first-run" },
          ],
        },
        {
          text: "Use Coterie",
          items: [
            { text: "Core concepts", link: "/guide/concepts" },
            { text: "Tasks and workspaces", link: "/guide/tasks-and-workspaces" },
            { text: "Configuration and permissions", link: "/guide/configuration" },
            { text: "Recovery", link: "/guide/recovery" },
            { text: "Troubleshooting", link: "/guide/troubleshooting" },
            { text: "Research claim demo", link: "/guide/demo" },
          ],
        },
      ],
      "/reference/": [
        {
          text: "Reference",
          items: [
            { text: "CLI commands", link: "/reference/cli" },
            { text: "Configuration", link: "/reference/configuration" },
            { text: "JSON and exit codes", link: "/reference/protocol" },
          ],
        },
      ],
    },
    socialLinks: [
      { icon: "github", link: "https://github.com/jolars/coterie" },
    ],
    editLink: {
      pattern: "https://github.com/jolars/coterie/edit/main/website/:path",
      text: "Edit this page on GitHub",
    },
    search: { provider: "local" },
    footer: {
      message: "Released under the MIT and Apache 2.0 licenses.",
      copyright: "Copyright © Johan Larsson",
    },
  },
});
