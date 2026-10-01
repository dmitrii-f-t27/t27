# NOW -- ball board v2: no link may claim strangers, the blog gets a door, every card names its spec (2026-10-01)

## specs/automation/ball-board.t27 v2 -- from the first live read (crm 6, mail 134, github 71)

- hh.ru was a counterparty on 50 unrelated mail matters. A domain link there would
  have filed all 50 under one client, so a domain link on a domain many strangers
  share (job boards, ATSs, mailbox providers, lists, submission systems) is now
  refused: `shared_domain`, `may_link`. An email link at the same domain is fine.
- Code work could reach a client only through a label or its repository, and the
  blog's PRs share their repository with everything else, named "blog: ...". The
  new `title` link claims work by the title's HEAD -- first word, lower-cased, cut
  at ':' or space -- so "fix(blog): typo" is not claimed by "blog". It is weighed
  after the label and before the repository.
- Owner's ask: every kanban card stands on a .t27 spec. `card_spec`: the path a
  title names wins; else the source's spec of record (CRM ->
  crm-client-workspace.t27, mail -> mail-push.t27); code work that names none has
  none, and the board COUNTS it instead of inventing a link.
- Found while testing: `t27c test-report` is BLOCKED on every spec that compares
  strings (`==` on `[]const u8` in the Zig backend) -- v1 of this spec and
  mail-push included. Their tests are exercised only by the host's binding tests.
- Claim unchanged: `RUN_LIVE = false`.
