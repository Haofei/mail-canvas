# Corpus Policy

MailCanvas keeps committed corpus files only when they are useful for
deterministic regression. Large one-off download batches belong in `runs/`, the
browser/artifact cache, or an external artifact store, not in git.

`catalog.json` is the committed golden set. `registry.json` is the lightweight
history of templates we have seen or tested. It records HTML and asset manifest
MD5s, aggregate asset size, whether the template is still retained in the repo,
and the latest pipeline result when available. `issues.json` records P0/P1/P2
rendering problems found by corpus runs; unresolved entries stay `pending`,
and rerunning a template after the issue disappears marks that entry `fixed`.
It also includes `summary.byType`, which counts repeated problem classes across
templates and runs so the highest-impact renderer fixes are visible.

## Tiers

- `golden`: small, representative, deterministic templates used by CI gates.
  Assets must be local or intentionally absent. Templates should cover classic
  open-source layouts, modern generated/editor output, and a few stable
  marketing examples.
- `registry`: compact metadata for research templates that were downloaded or
  tested but are not committed as fixtures.
- `runs/`: generated reports and temporary download batches. This directory is
  ignored by git.

## Promotion Rules

Promote a template to committed corpus only when it meets at least one of these
criteria:

- It protects a fixed renderer bug.
- It represents a widely used email generator or public template family.
- It covers a layout class not already represented in golden/research.
- It is needed to reproduce a high-value compatibility issue.

Do not commit templates just because they were downloaded by a scheduled job.
Keep the pipeline report, triage JSON, and first-bad crop; then promote only the
few templates that teach the renderer something durable.

## Workflow

Refresh the committed registry after changing the golden corpus:

```bash
npm run corpus:registry
npm run corpus:manifest
```

Run a temporary Really Good Emails batch without re-testing templates already in
`registry.json`:

```bash
npm run corpus:pipeline -- \
  --provider reallygoodemails \
  --collection latest \
  --limit 12 \
  --exclude-seen
```

For a growing local Really Good Emails mirror under the gitignored
`corpus/reallygoodemails/` directory, compare only templates whose HTML or
mirrored assets changed since the last local run:

```bash
npm run research:compare:local-rge-new -- \
  --work-dir /tmp/mail-canvas-rge-new
```

This writes `corpus/reallygoodemails/run-registry.json`, which is also
gitignored. The registry uses a content MD5 over the HTML plus local asset
manifest, so a template with the same name is rerun when its bytes change. Use
`--clear-seen-registry` when intentionally starting the local RGE run history
from scratch.

By default the pipeline removes newly vendored research HTML/assets from
`corpus/` after the run and leaves only the registry record. Pass
`--keep-vendored` when intentionally promoting or inspecting the downloaded
files before cleanup.

Really Good Emails intake also defaults to complete-asset mode: if any mirrored
image, stylesheet, font, or nested CSS asset returns 403/404 or cannot be
fetched, that template is skipped and any partial files are removed. Use
`--allow-incomplete-assets` only for manual research runs where placeholder
assets are acceptable.

Each pipeline run also writes `issues.json` in the run directory and updates the
committed `corpus/issues.json` unless `--no-issues-log` is passed.

If a research template should become a permanent regression fixture, copy only
that HTML and its `.assets/` directory into `corpus/`, add a single
`catalog.json` entry, then refresh `registry.json` and `manifest.json`.
