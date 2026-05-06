# Corpus

`manifest.json` is the committed template corpus index used by the Playwright
comparison scripts.

Fields:

- `name`: stable template id
- `url`: upstream catalog or source URL used for reporting
- `sourcePath`: optional committed local HTML fixture path
- `baseUrl`: optional base URL for resolving relative remote assets from local fixtures
- `provider`: source family
- `category`: coarse email category
- `supportTier`: `modern-supported`, `legacy-hacks`, or `invalid-structure`
- `status`: `active` or `known-warning`
- `expectedWarnings`: expected renderer warning count for known-warning fixtures
- `reason` / `supportReason`: scope and validation notes

Refresh it with:

```sh
npm run corpus:manifest
```
