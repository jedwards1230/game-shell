# Vendored panel assets

These files are served by the `tv-shell-panel` binary itself (embedded at compile
time via `include_str!`), so the panel renders with no CDN and no network — it
must work when the rest of the system is broken.

## `htmx.min.js`

- **Library**: [htmx](https://htmx.org/)
- **Version**: 4.0.0 (released 2026-08-28; a ground-up rewrite from the 2.x line -- see docs/PANEL.md and the migration PR that bumped this file for the attribute-rename and behavior-change audit)
- **Source**: `https://unpkg.com/htmx.org@4.0.0/dist/htmx.min.js` (published under npm's `next` dist-tag as of this vendoring -- `latest` was still 2.x, so pin the exact version, not `@next`, when re-fetching)
- **SHA-256**: `e484d9171a9db30a39c8f16e3d709d4137f3211c659f8e6125816635033d593f`
- **License**: BSD 2-Clause (compatible with this repo's GPL-3.0)

Committed verbatim from the official release — do not hand-edit. To update, fetch
the new release, re-verify its hash against the published artifact, and update the
version + hash above in the same commit.

## `style.css`

Hand-written admin stylesheet for the panel (dark, minimal). Not vendored.
