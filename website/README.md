# GFlick website

Marketing and documentation site for [GFlick](https://github.com/R4ULtv/gflick), the
experimental Logitech HID++ agent for Windows and macOS.

Built with Astro and Tailwind, output as a static site to `./dist/`.

## Structure

```text
/
├── public/            # favicon, _headers
├── src
│   ├── assets/fonts   # self-hosted Anton and Geist
│   ├── components     # section and UI components
│   ├── layouts        # Layout.astro, the shared page shell
│   ├── pages          # index, download, setup, devices, benchmark, privacy
│   ├── styles         # global.css, Tailwind theme and tokens
│   └── consts.ts      # repository and documentation links
└── astro.config.mjs
```

Shared links live in `src/consts.ts` - `REPO` is used in several places and the
documentation URLs derive from it, so a repository move is one edit.

## Commands

| Command        | Action                                   |
| :------------- | :--------------------------------------- |
| `pnpm install` | Install dependencies                     |
| `pnpm dev`     | Start the dev server at `localhost:4321` |
| `pnpm build`   | Build the production site to `./dist/`   |
| `pnpm preview` | Preview the build locally                |
| `pnpm lint`    | Run oxlint                               |
| `pnpm fmt`     | Run oxfmt                                |
