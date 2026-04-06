import { defineConfig } from "vitepress";

const base = (globalThis as { process?: { env?: { SITE_BASE?: string } } }).process?.env?.SITE_BASE ?? "/";

export default defineConfig({
  title: "pw-env",
  description: "Securely load environment variables from password managers",
  lang: "en-US",
  base,
  outDir: "../site",
  lastUpdated: true,
  cleanUrls: true,
  head: [
    ["link", { rel: "icon", type: "image/png", href: `${base}assets/images/Logo-pw-env@3x.png` }],
    ["meta", { name: "theme-color", content: "#f4f7fb" }],
  ],
  themeConfig: {
    logo: "/assets/images/Logo-pw-env@3x.png",
    siteTitle: "pw-env",
    nav: [
      { text: "Getting started", link: "/getting-started/installation" },
      { text: "Guides", link: "/guides/approvals" },
      { text: "Concepts", link: "/concepts/resolution-model" },
      { text: "Reference", link: "/reference/cli" },
    ],
    sidebar: [
      {
        text: "Getting started",
        items: [
          { text: "Installation", link: "/getting-started/installation" },
          { text: "First project", link: "/getting-started/first-project" },
          { text: "Shell integration", link: "/getting-started/shell-integration" },
        ],
      },
      {
        text: "Guides",
        items: [
          { text: "Migrating plaintext secrets", link: "/guides/migrate-secrets" },
          { text: "Approvals and trust", link: "/guides/approvals" },
          { text: "Updating pw-env", link: "/guides/update" },
        ],
      },
      {
        text: "Concepts",
        items: [
          { text: "Resolution model", link: "/concepts/resolution-model" },
          { text: "Configuration model", link: "/concepts/configuration-model" },
        ],
      },
      {
        text: "Reference",
        items: [
          { text: "CLI", link: "/reference/cli" },
          { text: "Configuration file", link: "/reference/configuration-file" },
        ],
      },
    ],
    search: {
      provider: "local",
    },
    socialLinks: [
      { icon: "github", link: "https://github.com/m42e/pw-env" },
    ],
    editLink: {
      pattern: "https://github.com/m42e/pw-env/edit/main/docs/:path",
      text: "Edit this page on GitHub",
    },
    footer: {
      message: "pw-env documentation",
      copyright: "MIT",
    },
    outline: {
      level: [2, 3],
    },
    docFooter: {
      prev: "Previous page",
      next: "Next page",
    },
  },
});
