# Handoff: give marsad its own data folder, and name launch files so people can read them

## Goal

Two changes, shipped as one release.

1. **Move marsad's data out of `~/.claude/marsad/` into `~/.marsad/`.**
   - `~/.claude/` belongs to Claude Code, and `CLAUDE_CONFIG_DIR` can move it.
   - marsad should later support other coding agents too, such as Codex (`~/.codex`), Gemini CLI (`~/.gemini`) or OpenCode. Their data can't sensibly live under Claude's folder.
   - The prompt library is plain instruction text that works with any tool, so it needs a home that doesn't belong to one tool.
2. **Give generated launch files names a person can read.** Today they look like `launch/269523ff7870ed80.md`. They should look like `launch/lead-coordination+output-hygiene-8873230f.md`, so a person recognizes them in a command, in `ps`, or in the folder.

## Decisions already made

- **The new folder is `~/.marsad/`** (`%USERPROFILE%\.marsad` on Windows).
  - It is not `~/.local/share` or `~/Library/Application Support`. People edit `prompts/` by hand, so the path must be easy to find and type. It follows the same pattern as `~/.cargo` and `~/.claude`.
  - `MARSAD_HOME` still overrides it.
- **Layout:**
  ```
  ~/.marsad/
    settings.json
    usage.db (+ -wal, -shm)
    update.json
    prompts/            the user's library (works with any tool)
    launch/             generated launch files
    generator/          the prompt generator's working folder
    cards/              share images when there is no Downloads folder
    claude/             Claude Code data: events.jsonl, events.1.jsonl, status/,
                        context-warned, statusline-original.json
  ```
  Each tool's data goes in its own subfolder, so `codex/` and others can be added later without moving anything again. Data shared across tools stays at the top: settings, usage, prompts, launch, cards and the update check.
- **Only Claude Code's own integration stays in `~/.claude`:** the hooks in `settings.json`, the `marsad-usage` skill and the status line command. Claude Code has to find those there.
- **Launch file names** follow `<role>-<keys>-<8 hex>.md`.
  - The role is `session`, `lead` or `teammate`.
  - The keys are the chosen prompt keys joined with `+`, cut to 60 characters on a character boundary.
  - The 8 hex characters are the first 8 of the FNV-1a hash that is used today.
  - The name must stay a pure function of the file's text. Then the same set reuses its file, and editing a prompt gives a new name, so a running lead's teammates' file never changes under it.
  - Pruning stays the same: teammates' files (prefix `teammate-`) are never pruned, and session and lead files are pruned after 7 days unused.

## Where the code is (start from `origin/main`; `main` moves, so re-check)

- **`src/main.rs` `data_dir()`:** returns `MARSAD_HOME`, otherwise `claude_dir().join("marsad")`. `claude_dir()` is `~/.claude`.
- **Callers of `data_dir()`:** each one needs a decision, shared folder or `claude/` subfolder.
  - `app.rs` (event log replay)
  - `hook.rs` (appends to events.jsonl)
  - `live.rs`
  - `context.rs` (`context-warned`)
  - `statusline.rs` (`status/`, `statusline-original.json`)
  - `store.rs` (`usage.db`)
  - `settings.rs` (`settings.json`, the legacy `history.json`)
  - `update.rs` (`update.json`)
  - `share.rs` and `shot.rs` (`cards/` fallback)
  - `rules.rs` (prompts, launch, prune, `remove_all`, the legacy files)
  - `prompt_gen.rs` (generator folder)
  - `install.rs`
- **Add a `claude_data_dir()`** (`data_dir().join("claude")`) for the per-tool files, so nothing builds that path by hand.
- **`src/rules.rs`:**
  - `files_for` builds the hash name.
  - `TEAMMATE_PREFIX` already exists.
  - `LEGACY_FILES` covers `coordination.md`, `output-hygiene.md` and `coordination+output-hygiene.md`: the paths earlier versions printed, kept current while they exist.
  - `run_coordination` prints the stable `coordination.md`.
- **Docs that name the old path:** `README.md` (settings, launch, cards, events.jsonl, usage.db), `src/store.rs` module doc, `assets/SKILL.md`, `CONCEPTS.md`, and tests that use `.claude/marsad` in example paths.

## Migration (the part that has to be right)

1. **On startup, before anything reads or writes data** (the dashboard, every CLI command, and `marsad hook`): if `MARSAD_HOME` is unset, `~/.marsad` doesn't exist, and `~/.claude/marsad` does, move the data across.
   - Rename files and folders into the new layout. On one volume, `rename` is enough; fall back to copy-then-delete across volumes.
   - Take a lock file while moving. The hook runs on every Claude Code event and can race the move, and a lost event or a half-moved `usage.db` is the failure to prevent.
   - A hook that finds the lock held must still deliver its event. Have it append to the old `events.jsonl`, and have the migration move that file last. Or keep a short wait-and-retry. Pick one and test the race.
   - Move the SQLite file together with its `-wal` and `-shm` files, with no connection open. Or open the database, checkpoint it, and then move it.
2. **Keep the old printed paths working for a few versions.**
   - `~/.claude/marsad/coordination.md`, `output-hygiene.md` and `coordination+output-hygiene.md` must still exist and stay current, because users have aliases and running sessions that point at them.
   - Keep writing those three at the old location while it exists. Nothing else stays in `~/.claude/marsad`.
   - Plan to remove them in a later release, and note that in the changelog.
3. **Hooks need no rewrite,** because they call the `marsad` binary. Still check that `marsad install` and `uninstall` handle both folders:
   - `install` creates `~/.marsad/prompts/`.
   - `uninstall` removes generated files in both places and never touches the user's `prompts/`.
4. **Make it idempotent.** Running it twice, or after a crash halfway through, must finish cleanly. If both folders exist, merge what is missing and never overwrite newer data in `~/.marsad`.
5. **Tell the user once.** For example, a footer note in the dashboard: "marsad's data moved to ~/.marsad". The CLI can print one line to stderr.

## How to verify

- **Unit tests with temporary folders:**
  - a fresh install
  - a migration from a populated old folder that includes `usage.db` with WAL files
  - a second run after the move, which does nothing
  - a crash after a partial move, which resumes
  - both folders present
  - `MARSAD_HOME` set, which means no migration at all
  - the legacy files staying current at the old path
- **Launch name tests:**
  - the format is `role-keys-hash8.md`
  - long key lists are cut at a character boundary
  - the same text gives the same name
  - an edited prompt gives a new name
  - teammates' files are still kept by pruning
- **The race:** a stress test that runs the migration while a loop runs `marsad hook` with sample events. Then count the events.
- **On this machine,** after the build: copy `~/.claude/marsad` to a scratch `MARSAD_HOME`-free test home (set `HOME` to a temporary folder), run the dashboard, and check:
  - usage history, badges and settings survived
  - `c` copies a command with a readable `~/.marsad/launch/...` path
  - an old alias to `~/.claude/marsad/coordination+output-hygiene.md` still starts a session
- **Agent team still works:** start a team lead from the new command in tmux. Check with a codeword in a user prompt that a teammate receives the teammates' file. This is the same probe that was used for v0.9.0: the lead is told nothing about files, and the teammate must answer with a codeword that only exists in the teammates' file.

## Repo conventions

- **Use a git worktree,** because another session may be working in the main checkout: `git worktree add ../marsad-data-folder -b feat/marsad-home origin/main`. Don't switch branches in the shared checkout.
- **Open a PR into `main`.** CI runs `cargo test --locked` on Ubuntu, macOS and Windows. Test the Windows path handling in the migration on purpose.
- **Release:** open a `chore/release-x.y.z` PR that bumps `Cargo.toml`, `Cargo.lock` and `CHANGELOG.md` with short user-facing bullets. Then push the `vX.Y.Z` tag, which runs the dist release workflow; the Apple Silicon build now uses `macos-15`. Then open a `chore: sync develop with main` PR.
- **Formatting:** the repo isn't formatted with one rustfmt config. Format only the lines you change, and don't run `cargo fmt` over the repo.
- **Docs:** update `README.md`, `assets/SKILL.md` and `CONCEPTS.md` for the new paths.
