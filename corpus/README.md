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
- `corpusGroup`: validation group:
  - `golden`: clean, vendored templates from stable GitHub/framework sources;
    this is the primary renderer compatibility corpus.
  - `real-world-dirty`: editor/community exports kept for diagnostics when their
    HTML or assets are not clean enough for golden gates.
  - `legacy-reference`: useful email-client compatibility references that rely
    on older hacks outside the modern renderer target.
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

Refresh the generated MJML official golden fixtures with:

```sh
npm run corpus:vendor-mjml
```

This uses `npx mjml@4.16.1` only while refreshing fixtures. The committed HTML
and mirrored assets are what CI uses.

Audit local fixtures for corpus issues that can distort visual comparisons:

```sh
npm run corpus:audit
```

Audit only the primary golden corpus:

```sh
npm run corpus:audit -- --corpus-group golden
```

The audit reports invalid inline `style="...url("...")..."` URL quoting and
empty linked CSS files, both of which should be treated as fixture quality
problems before using a high visual diff as renderer evidence.

Run visual comparisons for each quality group:

```sh
npm run compare:corpus
npm run compare:dirty
```
