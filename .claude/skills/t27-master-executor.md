---
description: Master execution tracker for the t27 merge queue — Wave Loop ladder, GF-T stack, and repo-wide CI state.
parameters:
  - name: action
    type: string
    description: "read | update | next-wave | check-queue"
---

# t27 Master Executor

This skill tracks the live merge queue and mechanical Wave Loop ladder for the t27 repo.
Update it at the end of every loop.

## Current status (2026-08-06)

### Wave Loop ladder
- **W881** — issue #1722, PR #1810 (`[581][2]^6 Pt`) — `MERGEABLE`, `BLOCKED` by required
  status checks, auto-merge enabled.
- **W882** — issue #1812, PR #1813 (`[583][2]^6 Pt`) — `MERGEABLE`, `BLOCKED` by required
  status checks, auto-merge enabled.
- **W883** — issue #1814, PR #1815 (`[585][2]^6 Pt`) — `MERGEABLE`, `BLOCKED` by required
  status checks, auto-merge enabled; implementation + close-out docs committed.
- **W884** — issue TBD, branch TBD (`[587][2]^6 Pt`) — ready, waiting for W883 to land.

### GF-T PR queue (Refs #1764)
All are `MERGEABLE`, `BLOCKED` by required status checks, auto-merge enabled:
- #1801 `feat/gft-argmax4`
- #1802 `feat/gft-classifier4`
- #1803 `feat/gft-exp2-layer4-synth`
- #1808 `feat/gft-training-demo`
- #1809 `integration/gft-full-stack`

### Known blockers
- GitHub Actions required checks are `expected` across the queue; auto-merge is the
  current mitigation.
- Pre-existing `corpus_classifier_matches_lean_completeness` failure for
  `specs/cloud/railway_deploy.t27` reproduces on `wave-loop-882` and may affect CI once
  runners are available. It is not introduced by any Wave Loop PR.

## Procedure

1. At loop start, read this skill and `.trinity/current-issue.md`.
2. After opening a wave PR, add the wave to this skill with state `mergeable/BLOCKED/auto-merge`.
3. After a PR lands, mark it `merged`, create the next wave issue/branch, and move the
   tracker forward.
4. When the GF-T queue moves, update the PR statuses here.

## Invariants

- Every commit references an issue (`Closes #N`, `Refs #N`, etc.).
- Do not hand-edit files under `gen/`.
- Do not merge with `--admin` when required checks are `expected`; use auto-merge.

phi^2 + 1/phi^2 = 3 | TRINITY
