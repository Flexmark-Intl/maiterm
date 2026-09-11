# maiterm.dev

Astro 5 + `@astrojs/starlight`. **Pushing any change under `website/` to `main` publishes
the site** — `.github/workflows/deploy-pages.yml` deploys to GitHub Pages on that path.
There is no staging step.

## Two halves, one site

| Route | Owned by | Chrome |
|---|---|---|
| `/features/*`, `/guides/*`, `/download/` | Starlight content collection (`src/content/docs/`) | Starlight — sidebar, Pagefind search, expressive-code |
| `/`, `/compare/*` | Hand-authored `src/pages/*.astro` | `layouts/Landing.astro` + `components/landing/*` + `styles/landing.css` |

File-based pages in `src/pages/` take route priority over Starlight's injected routes, so
the two coexist with no collision.

The landing page was a Starlight `template: splash` until 2026-09-11 — a docs page with the
sidebar hidden. That is why it read as documentation: the nav was a search box and a theme
dropdown with no destinations in it, the `h1` was the product name, the hero image was the
app icon, and the whole body was one narrow `.sl-markdown-content` column. **Astro was never
the constraint; Starlight was.** Don't propose migrating off Astro — the split is the fix.

## Rules

- **The gutter goes on the OUTER element** — `.ln-band`, `.nav`, `.foot` — never on the
  `max-width` inner. With `border-box` it comes out of the max-width instead, and past
  1180px every child silently insets by a full gutter. Invisible at 1024, obvious at 1440.
  This has been got wrong in both directions: once inset the nav, once made a shot render
  *narrower* than the column its `max-width` says it overhangs.
- **Both halves share the theme contract**: `data-theme` on `<html>`, persisted under the
  `starlight-theme` localStorage key. Starlight stores "auto" as an **empty string**, not
  `'auto'`. A toggle on one half must carry to the other.
- **Never force a theme, and never store one the visitor didn't choose.** Absence and `''`
  both mean "follow the system". Writing `'dark'` on first visit pins a light-system
  visitor to dark forever as though they'd picked it — that was the behaviour until
  `45e0d4b`. Starlight's own `ThemeProvider` already resolves
  `storedTheme || prefers-color-scheme`; don't override it.
- **The landing palette is `light-dark()` over `color-scheme: light dark`.** Each colour is
  declared once; `:root[data-theme=...]` narrows the scheme to force one side. `light-dark()`
  resolves to a *colour*, so a box-shadow varies its colour token rather than the whole value.
- **A docs table that names UI must quote the UI verbatim.** Renaming "task list" to "board"
  in prose once walked over `defaults.ts`'s rule name `'Keep a task list'` and the condition
  label `'Work is not on the task list'`, so the docs named rows that don't exist in
  Preferences. Call the feature what you like in prose; quote controls exactly.
- **`/compare/*` requires `theirEdge`** — a required field on the `Comparison` type naming
  something the alternative genuinely does better, so a comparison can't ship without one.

## Copy that has to stay accurate

- **Licence: source available, NOT OSI open source.** `LICENSE` carries a competing-use
  restriction. Say "free" and "source published"; never "open source".
- **It does phone home, a little.** The updater check reaches a Cloudflare Worker that
  counts one row per unique user per day (salted hash, salt rotates daily and is discarded).
  "No telemetry" would be false.
- **It is a kanban board, not a "task list"** — seven lanes, workstreams, real dependencies,
  assignment, an append-only log. See `docs/tasks.md`.

## The site drifts from the code, and nothing checks it

`features/tasks.md` documented six lanes and never mentioned `dropped` for a month after it
shipped; the commit that fixed that count in the root `CLAUDE.md` missed the website
entirely. Verifying marketing claims against the implementation has twice surfaced real app
bugs.

**When editing copy that asserts behaviour, verify against `src/` and `src-tauri/`, not
against `docs/`.** A design doc records intent; the code may never have fully landed it, or
may have moved since.

## Commands

```bash
npm run dev      # dev server
npm run build    # static build into dist/
npm run preview  # serve dist/ on :4321
```

HTML is served with `cache-control: max-age=600`, so a browser can show the pre-deploy page
for ten minutes after a successful deploy. Cache-bust with a query string before concluding
a fix didn't land.
