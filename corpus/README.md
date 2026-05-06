# Corpus

`manifest.json` is the committed template corpus index used by the Playwright
comparison scripts.

`catalog.json` is the vendored fixture index. Playwright and benchmark scripts
load template HTML from these local files so corpus runs do not depend on
upstream template hosts remaining available.

Fields:

- `name`: stable template id
- `url`: upstream catalog or source URL used for reporting
- `sourcePath`: optional committed local HTML fixture path
- `preserveLocal`: keep a committed generated/exported fixture instead of
  refreshing it from the upstream URL
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

Refresh vendored local fixtures with:

```sh
node scripts/vendor_corpus_templates.mjs
```
