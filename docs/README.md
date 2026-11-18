# Rulite docs

Static documentation site: Next.js (static export), Tailwind CSS v4, shadcn/ui components, light and dark themes.

```sh
npm install
npm run dev      # http://localhost:3000
npm run build    # static site in out/
```

Deployed to GitHub Pages by `.github/workflows/docs.yml` on every push to `main` that touches `docs/`; it sets
`DOCS_BASE_PATH` (e.g. `/rulite`) from the Pages settings. Build locally with the same base path to preview it:
`DOCS_BASE_PATH=/rulite npm run build`.
Pages live in `app/` (`page.tsx`, `install/`, `usage/`); the sidebar is `lib/nav.ts`.
