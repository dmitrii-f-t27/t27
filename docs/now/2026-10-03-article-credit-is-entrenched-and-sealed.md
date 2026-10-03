# NOW -- Article CREDIT is entrenched and sealed (2026-10-03)

Closes #5666

## What changed

- `docs/T27-CONSTITUTION.md` v1.3 gains **Article CREDIT**: the reward goes to the `.t27` spec author by GitHub login, or to the provider who supplied proof of compute on their own CPU, FPGA or GPU.
- A bee's commit is credited to the owner of the claimed key it ran under. An unclaimed key falls to the author of the PR.
- `bootstrap/stage0/CREDIT_HASH` seals the article. `bootstrap/build.rs` refuses to build `t27c` when the article and the seal disagree, when the heading is missing or appears more than once, or when text is added under it.

## What review 1 found

- A hidden copy of the article inside an HTML comment, placed above the real one, satisfied the seal while the visible text was changed. Now exactly one heading line may name the article, and it is compared as a whole line.
- Text added after the closing `---` was not sealed, although Markdown still shows it under the article. Now the sealed text runs to the next level-1 or level-2 heading outside a code fence.

## Not claimed

- A seal makes a change deliberate and visible, not impossible. The `t27-master-protection` ruleset requires 0 approvals and no code-owner review.

## What review 2 found

- A hidden copy could still sit under a heading the duplicate count missed: a setext heading, an `<h2>` tag, a zero-width space or a Greek capital iota inside "CREDIT", or a collapsed `<details>`. The charter now refuses raw HTML, invisible formatting characters, setext headings and non-ASCII letters in headings, so nothing in it can hide or disguise a copy.
- A `## ` indented four spaces or by a tab ended the sealed text, though Markdown shows it as code. Only a line indented by at most three spaces, with no tab, now counts as a heading or a fence.
- The article gains point 4: no other text in the charter overrides it.
