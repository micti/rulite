# Rulite docs

Static documentation site: Next.js (static export), Tailwind CSS v4, shadcn/ui components, light and dark themes.

```sh
npm install
npm run dev      # http://localhost:3000
npm run build    # static site in out/
```

Set `DOCS_BASE_PATH=/rulite` when the site is served from a sub-path (e.g. GitHub Pages).
Pages live in `app/` (`page.tsx`, `install/`, `usage/`); the sidebar is `lib/nav.ts`.
