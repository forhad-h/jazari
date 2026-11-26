# Handoff: jazari work, week of 2026-11-30

> **Delete this file when every item in "Open work" is done or dropped.**
> The lead removes it in its own PR (`chore: remove the handoff`), after the founder agrees.

This file restarts the work that stopped on 2026-11-26. It is written for an agent team: one lead
and several teammates. The lead reads the whole file. Each teammate reads "Rules for everyone" and
its own track.

## How to start

Two pull requests from 2026-11-26 are still open, and their CI runs are not checked yet:

- **#139** adds this file. Until it merges, read this file with `gh pr diff 139` or
  `git show origin/agent-lead-handoff:HANDOFF.md`.
- **#137** is the 1.0.9 release.

The lead's first job: check CI on #139 and #137, merge #139, then release 1.0.9 from #137 (see
"How to release").

Start the lead from the repo, in a terminal outside any Claude Code session:

```sh
cd ~/Projects/marsad
jazari claude --team
```

Give the lead this first message:

> Read `HANDOFF.md` and `AGENT-BOARD.md`. You are the lead. Compare the section "State on
> 2026-11-26" with GitHub (`gh pr list`, `gh release list`). Then start one teammate for each
> track in "Open work" that is ready.

When the lead starts a teammate, the spawn prompt names the track, for example "Track B". The
prompt also tells the teammate to read "Rules for everyone" and that track in `HANDOFF.md`.

## State on 2026-11-26

- **Product:** jazari, an AI software company harness and a dashboard for Claude Code sessions.
  The repo is `github.com/forhad-h/jazari`, and it is **public**. Its old name was marsad.
  Versions before 1.0.0 are old marsad releases.
- **Latest release:** 1.0.8. `main` also holds #131 (dashboard answers with Touch ID) and #135
  (only the installed jazari refreshes the hooks). Release PR #137 turns them into 1.0.9. It is
  open and not merged.
- **Founder's machine:** on 1.0.7 or later. The Telegram bridge runs as a launchd service, and
  `jazari update` restarts it by itself.
- **Open pull requests:** #136 (Build view, draft) and #138 (guard false positives, draft).
  Check `gh pr list` for anything newer.
- **Plans** are in `docs/plans/2026-11-26-*.md`. Each open track names its plan.

## Rules for everyone

### Roles

- **The lead** reviews every pull request, merges it, and makes every release. The lead writes no
  feature code.
- **A teammate** never merges, tags or releases. When a pull request is ready, the teammate sends
  the lead its number and the commit hash.

### Coordination

- Read `AGENT-BOARD.md` at the repo root before any work. Register in **Agents**, and claim files
  in **Claims** before you edit them. Release your claims when your pull request merges.
- The **Decisions** on the board are binding.
- Never claim `README.md` or `CHANGELOG.md` exclusively. Every pull request adds its own lines, and
  the pull request that merges second rebases.

### Git and checks

- Work in a fresh worktree off `origin/main`, on your own branch. Open one pull request to `main`.
- Never commit to `main`. Never force-push a branch that is not yours.
- Before you send a pull request, both checks must pass:
  - `cargo test --locked`
  - `cargo clippy --all-targets`, with 0 warnings
- CI runs the tests on Linux, macOS and Windows. It must be green on all three.
- **After every rebase onto a release,** run `git diff origin/main -- CHANGELOG.md`. Your bullets
  must sit under `## Unreleased`. Git can put them inside the released `## X.Y.Z` section with no
  conflict. This happened three times on 2026-11-26.
- Read `CLAUDE.md`, and never run `cargo fmt`.

### Safety

- **Development data:** use `JAZARI_HOME=<scratch dir>`. jazari ignores `MARSAD_HOME`.
- **Live tests with a Claude session:** use `CLAUDE_CONFIG_DIR=<scratch dir>`, so the session
  never loads the real `~/.claude/settings.json`. On 2026-11-26 a debug build rewrote the founder's
  hooks this way. Release 1.0.9 stops that, but keep the rule anyway.
- Never run `jazari install` or `jazari uninstall` while you develop.
- Before a large build or test run, run `jazari watch system --wait`. Several sessions share this
  machine.
- Ask the founder before you change the founder's machine or spend money. Also ask before you
  reach outside, for example a deploy or a message to a person.

### Founder rules (binding)

1. **Telegram scope:** Telegram can control only jazari's own sandbox work: products, tasks,
   worker questions and approvals, and the supervisor. Sessions the founder starts by hand are
   controlled only at the computer, from the dashboard. Telegram gets no commands for them and no
   notices about them.
2. **Telegram pushes:** Telegram sends a message only when the founder must decide or act. Status
   is given only when the founder asks (`/status`, `/tasks`). A test lists every push with its
   reason.
3. **Presence check:** every approval from the dashboard needs Touch ID or the Mac password.
   A deny needs no check. On Linux the dashboard can only deny.
   - For a session's permission prompt, the check runs inside the hook process.
   - For the Build view, the check runs in the dashboard before each write.
4. **Writing style:** messages to the founder use Easy English, and they are short and easy to
   scan.
5. **Disagreement:** when the founder asks for something that looks wrong, give a short reason
   and a better option. Do not just follow.

## Open work

The tracks in this section can run in parallel, except where an item says it waits. `src/app.rs` and
`src/ui.rs` (the dashboard) allow one track at a time, in this order: A, then B.

### Track A: finish the Build view (PR #136)

- **Plan:** `docs/plans/2026-11-26-build-view.md`
- **Branch:** `agent-build-view`, draft PR #136.
- **Left:**
  1. Add the README key list entry and a section, and a CHANGELOG line under `## Unreleased`.
  2. Run a live Touch ID check of deny and approve, with a scratch `JAZARI_HOME`.
  3. Mark #136 ready and send it to the lead.
- **Files:** `src/build_view.rs`, `src/build_ui.rs`, `src/jazari/dashboard.rs`, small hooks in
  `src/app.rs` and `src/ui.rs`.

### Track B: dashboard part of per-project prompts and skills (waits for Track A)

- **Plan:** `docs/plans/2026-11-26-project-prompts-and-jazari-claude.md`. The command-line part
  shipped in 1.0.8.
- **Left:**
  1. Show the project's set on its session rows.
  2. In the `c` picker, `s` saves the choice for the project.
  3. In the `c` copy-path mode, the project's set is selected by default.
  4. If it is simple, move `RulesPicker` from `src/app.rs` into a module that `src/pick_term.rs`
     can share. If it is not simple, leave it.
- Use `project::set_for` for the copy-path default and the `s` key.

### Track C: guard false positives on text

- **Problem:** the founder gets false permission questions, such as "`jazari update` here
  installs or changes jazari". They come when the words only appear in text. That text can be a
  heredoc body, a `--body` string or a quoted argument, for example a pull request description.
- **Fix:** match jazari's own commands only where a command starts. Split on `&&`, `||`, `;`, `|`
  and newlines outside quotes and heredoc bodies. If parsing is unsure, keep today's behavior
  (ask).
- **Tests:** the heredoc and PR-body cases do not ask. `jazari update`, `cd x && jazari update` and
  `"/path/jazari" update` still ask.
- **Files:** `src/act.rs`, and `src/risk.rs` if it matches text the same way.
- **State:** done in draft PR #138 (branch `agent-self-cmd`). `src/act.rs` now splits a command
  line into simple commands. It skips quoted text, heredoc bodies and comments, and it still reads
  `$(…)`, backticks, `sh -c`, `eval` and heredocs that a shell runs. 524 tests pass. `src/risk.rs`
  is not changed.
- **Left:**
  1. A live check in a scratch session (`CLAUDE_CONFIG_DIR=<scratch>`).
  2. The lead reviews #138, then it is marked ready and merged.
  3. Decide whether `src/risk.rs` needs the same fix.

### Track D: answer plan, Part B (sandbox auto-answers)

- **Plan:** `docs/plans/2026-11-26-answer-prompts-from-jazari.md`, Part B.
- **Left:** jazari answers routine questions from its own sandbox workers. Build the deterministic
  check in a new `src/jazari/routine.rs`.
  - Deny by default. A question that touches deploy, spend, contact, secrets, user data, security,
    payments, deleting data or network access goes to the founder. When unsure, it goes to the
    founder.
  - The check reads the answer options too.
  - An auto-answer picks only the worker's own "Recommended:" option. With no recommended option,
    the question goes to the founder.
  - Each auto-answer is logged and shown on the dashboard as "answered by jazari". It is not
    pushed to Telegram.
- **Also open:** approving a plan (ExitPlanMode) from the dashboard. A plain "allow" does not
  approve it; find the hook output that does.

### Track E: free memory by closing apps (waits for the founder's go)

- **Plan:** `docs/plans/2026-11-26-free-memory-by-closing-apps.md`, including the review changes:
  - Never close the frontmost app or a call app.
  - Count an app's memory together with its helper processes.
  - Send only the approval card to Telegram.
- Ask the founder before you start building it.

### Track F: hardening follow-ups

- **Plan:** `docs/plans/2026-11-26-jazari-hardening-followups.md`
- **Left:**
  1. An egress proxy for the worker containers. Today the containers have open network access.
  2. marsad's risk hook inside the container.
  3. A copy of the audit head that this machine cannot change.
  4. The tell inbox: any session of the same user can add a fake "user" message to another
     session's inbox. The socket approach from the answer feature can fix this.
  5. A newer pinned Claude Code version in `assets/jazari/Dockerfile` (now 2.1.332).
- Each item is its own pull request. Item 4 matters most.

### Later (do not start without the founder)

- `jazari claude --remote`: Telegram control of one session that jazari started, as a per-project
  opt-in. It needs the same socket protection as the answer feature.
- Opt-in Telegram control of sessions started by hand. This feature needs the founder's explicit choice.

### For the lead

- Releases 1.0.7 and 1.0.8 have no `dist-manifest.json` asset (1.0.6 had one). `jazari update`
  does not need it, but find out why the `host` step leaves it out.
- Ask the founder about two old items: keep them, or drop them?
  - The old marsad `develop` branch has uncommitted `/rename` work in `~/Projects/marsad`.
  - The branch `agent-tile-rewards` was never pushed.
- Old merged remote branches can be deleted after the founder agrees.

## How to release (lead only)

1. Merge the feature pull requests (squash) after CI is green on all three systems.
2. In a fresh worktree off `origin/main`:
   - Bump `version` in `Cargo.toml`.
   - Run `cargo build --offline`, so that `Cargo.lock` changes too.
   - Rename `## Unreleased` to `## X.Y.Z - YYYY-MM-DD`.
3. Commit `chore(release): X.Y.Z`, open a pull request, and wait for CI. Then squash-merge it.
4. Tag the merge commit `vX.Y.Z` and push the tag. The `Release` workflow (`dist`) builds the
   assets.
5. The `host` step often fails on a GitHub artifact download. Run
   `gh run rerun <id> --failed` once.
6. Check that the release has `jazari-installer.sh` and `jazari-installer.ps1`.

## Traps to know

- **Hook ownership:** since 1.0.9, any hook command that runs a file named `jazari` belongs to
  jazari, and a refresh replaces it. A user's own wrapper script needs a different name.
- **The answer feature is off by default.** The founder turns it on with
  `jazari watch answer --on`, in a terminal outside Claude Code. A session cannot turn it on.
- **Windows** builds in CI, but nobody has tested it live. Paths and line endings broke Windows
  CI several times. Use `std::env::temp_dir()` and `Path::join` in tests.
- **Squash merges and stacked branches:** a stacked branch conflicts after the base is
  squash-merged. Rebase with `git rebase --onto origin/main <old base commit>`.
- **Message delivery:** a message to another session arrives only when that session runs its next
  tool call. Since 1.0.5, an idle session also gets a wake-up.
