# Handoff: disk guard and cleanup, so a full disk never hangs the machine

## Goal

A full disk has hung this Mac twice while Claude Code sessions were running unattended. When the
disk is full, macOS can't grow swap, Claude Code can't write its transcript, and the machine stops
responding. marsad should:

1. **Watch free disk space** and notify the user at 80%, 90% and 95% full.
2. **Stop Claude Code sessions at a safe point** once the disk passes 95%, before the machine hangs.
   This must work when nobody is at the desk and the dashboard isn't open.
3. **Help clean up regularly**: find stale data that can be rebuilt (caches, build output, unused
   Docker data, old backups, finished worktrees), say how much each frees, and remove it safely.
   Claude judges the unclear items. marsad runs the deletions.

Build this in steps (see "Suggested order"). Steps 1–2 matter most.

## What a real cleanup found (2026-11-23, 228 GB disk)

Before the cleanup the disk was 95% full with 10 GB free. A manual cleanup brought it to 48% full
with 105 GB free. These are the real numbers to design and test against:

| Where | Size | What it was | Action taken |
|---|---|---|---|
| Docker build cache | 32.8 GB | none in use | `docker builder prune -a -f` freed 27.7 GB |
| Docker images | 42.9 GB, 76 images | 52 used by no container | `docker image prune -a -f` |
| Docker stopped containers | 0.8 GB | | `docker container prune -f` |
| Docker volumes | 30.3 GB, 206 volumes | 199 unattached once stopped containers were gone | `docker volume prune -a -f` freed 24.5 GB (**deletes data**, user confirmed) |
| `~/Library/Containers/com.docker.docker` (Docker.raw) | 100 GB, now 21 GB | Docker's disk image | **doesn't give space back right away**: freed bytes came back later or after Docker restarted |
| `~/.npm` | 7.2 GB | npm cache | `npm cache clean --force`, which leaves `~/.npm/_npx` (1.4 GB) |
| `src.bak-*` copies of one project | 8.4 GB, 7 × 1.2 GB | backups from Nov 19–22 | deleted the 6 oldest, kept the newest |
| `<project>/.next` | 1.9 GB | Next.js build output | deleted |
| `<project>/.claude/worktrees/*` | ~5.8 GB, 9 worktrees | Claude Code worktrees, each with its own `node_modules` and `.next` | not touched: some branches may hold unmerged work |
| `node_modules` in idle projects | ~8 GB | | not touched |
| `~/Library/Application Support/Claude/vm_bundles` | 10 GB | Claude desktop's VM image | managed by the app: report it, never delete it |
| `~/.claude/projects` | 1.4 GB | Claude Code transcripts | not touched |
| `~/.local/share/claude/versions` | 1.0 GB | old Claude Code CLI versions | not touched |

The macOS Storage panel showed "Documents 46 GB" while `~/Documents` held only 17 MB, and showed
"System Data 66 GB". macOS sorts files by kind, not by folder, so the cleanup view must list
**real paths**, not categories.

## Decisions already made

- **Two thresholds checked together.** A percentage alone is wrong on large disks: 95% of 2 TB still
  leaves 100 GB. Each level is a **percent used** and a **minimum free GB**, and whichever fires
  first wins. Defaults: notice at 80% / 30 GB free, warning at 90% / 15 GB, critical at 95% / 8 GB,
  emergency at 98% / 3 GB. All of them can be set in `~/.marsad/settings.json`.
- **Measure the volume that holds the user's files**, not `/`. On macOS that is
  `/System/Volumes/Data`. Find the right one through `sysinfo::Disks`, using the longest mount
  point that contains `$HOME` and each session's `cwd`. Count **available** space (`f_bavail`), not
  free space, and don't count APFS purgeable space as available.
- **Halting is graceful, never a kill.**
  - At **critical**, a synchronous `PreToolUse` hook (see below) returns
    `{"continue": false, "stopReason": "…"}`. The session stops before its next tool call, its
    state is saved, and it can be resumed with `claude --resume`. The `stopReason` says why and
    how much space is free.
  - At **warning**, the synchronous `UserPromptSubmit` hook and the `PreToolUse` hook add
    `additionalContext` telling Claude the disk is almost full: finish the current step, don't
    start large builds, image pulls or installs, and write a handoff now. This uses the same
    mechanism as the handoff warning in `context.rs`.
  - At **emergency**, while a long tool call is running (for example a Docker build that fills the
    disk and fires no hooks for minutes), the watcher sends **SIGINT** to the Claude Code process,
    the same as pressing Esc. The pid is already in the event log (`CLAUDE_PID`, see `hook.rs`).
    Never SIGKILL. On Windows, fall back to the notification only.
  - Sessions that marsad halted get a `halted: disk` mark, so the dashboard shows the reason and a
    resume command.
  - Before relying on it, check `continue: false`, `stopReason`, `additionalContext` and the
    exit-code behaviour against the current Claude Code hooks reference (code.claude.com/docs).
    Hook semantics change; see `docs/solutions/` for earlier surprises.
- **The guard must not need the dashboard.** Alerts today run only while the TUI is open
  (`alerts.rs`). The guard has two parts:
  1. **The synchronous hook itself checks the disk.** It's one `statvfs` call and takes
     microseconds. Every tool call of every running session then checks, which covers the
     unattended long run, because a running session makes tool calls constantly. Cache the reading
     in `~/.marsad/claude/disk.json` for about 5 s so a burst of calls doesn't repeat the check.
  2. **A small watcher, `marsad guard`,** started detached by the `SessionStart` hook if one isn't
     already running (use a pid file plus a lock). It checks every 15–30 s, sends notifications
     (reuse `alerts::notify` and sound), and does the SIGINT step. It exits on its own once no
     live sessions have been left for about 10 minutes. Don't install launchd or systemd units:
     they would make `marsad install` and `uninstall` more invasive, and the hooks already start
     the watcher whenever it's needed.
- **Each level is notified once per crossing** (with hysteresis, re-armed when usage drops 2 points
  below the level), not every time the disk is checked.
- **Optional phone notification**, off by default because nothing leaves the machine unless the
  user turns it on: a user-set `ntfy` topic or webhook URL in settings, sent only at warning and
  above. The point is to reach the user who has left the desk.
- **Optional ballast file**, off by default: `~/.marsad/ballast`, a few GB set by the user (written
  with real blocks, not a sparse file). The guard deletes it at emergency so there is room for a
  clean stop and for the cleanup, and recreates it once space is back above the warning level.
  This is the standard "reserve space" trick for disk-full hangs.

## Cleanup design

### Who does what

- **marsad scans and measures, and runs deletions.** Scanning is quick, measured with sizes on
  disk, and can be repeated. Every deletion is a fixed **action** from a built-in list, for
  example `docker-builder-prune`, `npm-cache-clean` or `remove-dir <path>`. A model never writes
  the shell command that runs.
- **Claude judges.** Ambiguous items (is this `*.bak` folder stale? does this Docker volume hold a
  database the user cares about? is this worktree's branch merged?) go in one redacted batch to
  `claude -p`, the same way `prompt_gen.rs` runs it (same marker env, so it isn't counted as a
  session). Claude returns, for each item, keep / suggest / safe plus a one-line reason. The user
  reviews and confirms in the TUI.
- **Claude in a session** gets `marsad disk` (status) and `marsad clean --plan` (JSON) through the
  `marsad-usage` skill (`assets/SKILL.md`). Add a skill rule: before a large build, Docker pull or
  install, check `marsad disk`.

### Safety tiers

| Tier | Examples | Behaviour |
|---|---|---|
| **A: rebuilt automatically, no data** | Docker build cache; dangling images; stopped containers older than 7 days; npm, pnpm, yarn, pip and uv caches; `brew cleanup`; `.next`, `target/`, `dist/` and `.turbo` in projects with no live session; Xcode DerivedData | Can run automatically once the user turns auto-clean on. Also run at critical even if auto-clean is off, but only these. |
| **B: rebuilt, but slow to get back** | `node_modules` in projects idle more than N days; Docker images no container uses; old Claude Code CLI versions (keep the current one and one previous) | Always proposed, run after one confirmation. |
| **C: holds data** | Docker volumes; `*.bak*` and `*-backup*` folders; worktrees; Claude transcripts; anything Claude marked unsure | Always proposed one item at a time. Show what's inside (for a volume, the containers and images that last used it). Never automatic. |
| **Never** | Source files, `.git`, `.env*`, worktrees with uncommitted changes or unpushed commits, anything inside a live session's `cwd`, app-managed data (`vm_bundles`, Docker.raw itself) | Report only. |

### Use what marsad already knows

marsad already knows which projects have live sessions (event log) and when each project was last
active (`usage.db`). "Idle" and "live" come from these, not from file timestamps, which
`node_modules` installs and git operations keep refreshing. No other cleanup tool has this signal.

### When cleanup runs

- **After a task finishes** (on the `SessionEnd`, or `Stop` with nothing running, hook, through
  the detached `marsad sync` route): look only at that session's project. That means worktrees the
  session made under `.claude/worktrees/` whose branch is merged into the default branch and has a
  clean tree, the session's scratchpad under the system temp folder, and build output if the
  project now has no live session. Tier A runs if auto-clean is on. Everything else is added to a
  "cleanup ready" list.
- **Regularly**: once a day when the dashboard opens, or from the watcher, scan the known places
  below and update the list. Run nothing from it without the user.
- **On a threshold**: at warning, scan right away and put the largest safe items in the
  notification ("Disk 91% · Docker build cache 30 GB can be freed · open marsad, key `D`").

### Places to scan (macOS first, with Linux and Windows fallbacks)

- **Docker:** use `docker system df -v`, and only when the `docker` CLI and daemon respond. Report
  that Docker.raw may not shrink until Docker restarts. If it stays large after pruning, point
  the user to Settings → Resources → Virtual disk limit.
- **Package caches:** `~/.npm` (the `_npx` part separately), `~/Library/Caches/pnpm` /
  `~/.local/share/pnpm/store`, `~/Library/Caches/Yarn`, `~/.cache/pip`, `~/.cache/uv`, the
  Homebrew cache, `~/.cargo/registry` and `~/.cargo/git`, `~/.gradle/caches`, `~/.m2`.
- **Projects:** walk the parent folders of `cwd`s marsad has seen (for example `~/Projects`) for
  `node_modules`, `.next`, `target`, `dist`, `build`, `.turbo` and `.venv`, plus folders named
  `*.bak*`, `*backup*` or `*-old` that sit next to a folder with the same base name.
- **Claude Code:** `<repo>/.claude/worktrees/*` (check with `git worktree list`; whether each
  branch is merged; `git status`), `~/.local/share/claude/versions`, `~/.claude/projects`
  (report only: Claude Code's `cleanupPeriodDays` setting controls it), and the scratchpad folders
  under the system temp folder.
- **Toolchains:** unused `~/.rustup/toolchains`, `~/.nvm/versions` (keep the default one), Xcode
  `DerivedData`, `~/Library/Developer/CoreSimulator` (through `xcrun simctl delete unavailable`).
- **System (report only, with the command to run):** Time Machine local snapshots
  (`tmutil listlocalsnapshots /`), which are often the hidden part of "System Data"; the Trash;
  `~/Library/Caches` broken down by app.

### Doing it safely

- Measure before and after each action, and record the **bytes freed** with a timestamp in
  `usage.db` (a new `cleanups` table). The dashboard can then show "freed this month".
- Moving a file to the Trash frees no space, so tier A/B items are deleted outright. Tier C items
  are deleted only after the user confirms each one.
- Check every target again right before deleting it: the path still exists, it isn't in a live
  session's `cwd`, and the worktree is still clean. Then delete with the Rust std library. Don't
  run `rm -rf` through a shell, and refuse paths outside `$HOME` and the system temp folder.
- Run actions one at a time on a worker thread that reports through the channel read in `tick()`
  (the existing rule in `CLAUDE.md`).

## Suggested order (each step is one release)

1. **`marsad disk`** (status and JSON), the threshold settings, the check in the hook, and the
   `PreToolUse` synchronous guard with warning context and the critical stop. That alone would have
   prevented both hangs.
2. **`marsad guard`** watcher: notifications without the dashboard, emergency SIGINT, ballast,
   phone notification.
3. **Cleanup scan and plan**: a `D` key in the TUI with sizes by tier, `marsad clean --plan`, and
   tier A/B actions.
4. **Claude judging tier C**, cleanup after a task, and the freed-bytes history.

## Where the code is (start from `origin/main`)

- `src/install.rs`, `EVENTS` and `add_hooks()`. Every event is registered `async` except the
  `UserPromptSubmit` context hook. Add a **synchronous `PreToolUse` entry** (`marsad guard --hook`,
  timeout 5) next to the async logging one. It must return within milliseconds and never fail
  closed: any error means it prints nothing and exits 0. `is_ours()` and `strip` must find it so
  `uninstall` removes it.
- `src/context.rs`, `hook()`. The existing synchronous hook that returns
  `hookSpecificOutput.additionalContext`. Copy its shape and its pid and session lookup.
- `src/hook.rs`. The event logger, `CLAUDE_PID`, and the detached spawn (`detached()`) to reuse
  for starting the watcher.
- `src/alerts.rs`. `notify()`, the sounds and `detached()`. Use the same helpers outside the TUI.
- `src/settings.rs`. Settings in `~/.marsad/settings.json`.
- `src/store.rs`. `usage.db`, for the project last-active times and the new `cleanups` table.
- `src/prompt_gen.rs`. How marsad runs `claude -p` (marker env, working folder).
- `sysinfo` 0.39 is already a dependency (`Disks`, `System`).
- Read `CLAUDE.md` and `docs/solutions/` before changing hooks. Keep `MARSAD_HOME` scratch dirs in
  development. Update README, CHANGELOG, `assets/SKILL.md` and the footer key hints.

## Tests

- Thresholds: percent versus minimum GB, hysteresis, and once-per-crossing notification, against
  a fake disk reading that can be injected, like `share::Desktop::for_tests()`. Tests never fill a
  real disk.
- Guard hook output at each level, including that an error gives empty output and exit 0.
- Install and uninstall add and remove the new synchronous entry and keep other tools' hooks.
- Cleanup planning against a temp folder tree: tiers are assigned correctly, live-session `cwd`s
  and dirty worktrees are excluded, and paths outside the allowed roots are refused.
- Docker and `claude -p` go through injectable runners, so tests never call them.

## Done when

- With a fake reading of 96%, a running session stops before its next tool call with a clear
  reason, and resumes normally once space is freed.
- With the dashboard closed, crossing 80%, 90% and 95% each sends exactly one desktop notification.
- `marsad clean --plan` on this machine lists the kinds of items in the table above, with sizes and
  tiers, and nothing in the "Never" row is ever offered for deletion.
