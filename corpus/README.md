# Corpus Policy

MailCanvas keeps committed corpus files only when they are useful for
deterministic regression or repeatable research. Large one-off download batches
belong in `runs/` or an external artifact store, not in git.

## Tiers

- `golden`: small, representative, deterministic templates used by CI gates.
  Assets must be local or intentionally absent. Templates should cover classic
  open-source layouts, modern generated/editor output, and a few stable
  marketing examples.
- `research`: real templates worth keeping for compatibility analysis. These
  can be noisy or have known gaps, but should still be reproducible with local
  assets.
- `dirty`: legacy, malformed, or browser-repair-dependent templates. These are
  useful for investigation but should not define product quality.
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
