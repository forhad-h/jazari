# Prompts and skills per project, and a `jazari claude` launcher

Status: the command-line part is built: `jazari build project`, `jazari claude` and the handoff.
The dashboard part (`src/app.rs`, `src/ui.rs`) comes after the answer view (#131), because #131
changes the same files.

## Goal

Today the founder ticks one set of prompts and skills for every project (the `c` picker saves it as
the global ticked defaults). Different projects need different sets: a Rust tool needs other skills
than a web app. Starting a session with the right set also takes two steps: copy the command with
`c`, then paste it in a terminal.

This plan adds:

1. **A set per project.** jazari stores the chosen prompts and skills for each project in its own
   data folder, never in the repo.
2. **`jazari claude`.** One command that starts Claude Code in the current folder with the project's
   set, or with the global defaults when the project has none.

## What a project is

A project is the main checkout of a git repo:

1. jazari runs `git rev-parse --git-common-dir` and takes the folder that holds the `.git` folder.
   So all worktrees of one repo share one project. (`--show-toplevel` would give each worktree its
   own project. The board uses `--git-common-dir` for the same reason.)
2. Outside git, or when git is missing, the project is the current folder.
3. jazari uses the canonical path (symlinks resolved), so the same repo opened through two paths is
   one project.

## Where the set is stored

`<data dir>/projects/<name>-<hash>.json`, for example `~/.jazari/projects/shop-3f9a1c2e.json`:

- `<name>` is the project folder's name, cut to a safe file name, so a person recognizes the file.
- `<hash>` is the first 8 hex digits of the FNV-1a hash of the canonical path (`rules::fnv1a`). It
  keeps the name stable and unique.

```json
{ "path": "/Users/me/Projects/shop", "keys": ["coordination", "easy-english"], "updated": 1795690000 }
```

The file never goes into the repo, so a set is personal and can't leak through a commit.

## Commands to manage the set: `jazari build project`

The prompt and skill library is managed under `jazari build` already (`build rules`, `build prompt`,
`build skill`). A project's set is a choice from that library, so it lives next to them. The top
level stays for the lifecycle (`install`, `update`) and for `jazari claude`.

```sh
jazari build project                  # the project of this folder, its set, and where it comes from
jazari build project edit             # the same picker as `c`; saving writes the project's set
jazari build project add <key>        # add a prompt or skill
jazari build project remove <key>     # remove one
jazari build project clear            # delete the project's set: the global defaults apply again
jazari build project list             # every project with a saved set
```

Every command takes `--path <folder>` to work on another project. `jazari build project` also
prints "Working with jazari" as the first line, marked "(always)", because every launch file starts
with it (#127).

`add` refuses a key that is not in the library, with the list of keys, like `jazari build rules`.

## `jazari claude`

```sh
jazari claude                   # start Claude Code here with the project's set
jazari claude --pick            # choose the set for this start only, in the picker
jazari claude -- --model opus   # anything after `--` goes to claude unchanged
```

Steps:

1. Find the project of the current folder.
2. Take its set. Without a saved set, take the global ticked defaults.
3. Skip keys that are no longer in the library, with one warning line:
   `jazari: left out "old-prompt" (no longer in your library); jazari build project remove old-prompt cleans it up`.
   An unknown key never stops the start.
4. Write the launch file with `rules::ensure`. "Working with jazari" is always first, once
   (`rules::files_for`, #127).
5. Install the chosen skills where Claude Code loads them, with `skills::provide`, as `c` does.
6. Make a new random (v4) session id, and log the set as a `JazariLaunchSet` event under that id,
   so the dashboard shows it from the session's first second (as the handoff does, #122).
7. Print one line with the set in use and where it comes from, for example:
   `jazari: starting with jazari (always), coordination, easy-english (project shop)`.
8. Start `claude --session-id <id> --append-system-prompt-file <file> <args>` in the current folder.
   - macOS and Linux: `exec`, so Claude Code takes over the terminal and gets its signals.
   - Windows: `exec` doesn't exist there. jazari starts `claude` (or `claude.cmd`), waits, and exits
     with the same exit code.

`--pick` opens the `c` picker in the terminal before step 4, with the project's set ticked. The
picker's last step has a choice "use for this start only" (the default) or "save for this project".

If `claude` is not found, `jazari claude` says so and prints the command, so the founder can start it
by hand.

### Arguments that conflict

`jazari claude` sets `--session-id` and `--append-system-prompt-file` itself. If the arguments after
`--` hold either one, jazari stops with a message, because Claude Code reads only the last
`--append-system-prompt-file`, and a second session id would break the dashboard's link. `--resume`
and `--continue` are passed on unchanged, and then jazari doesn't set `--session-id`, because a
resumed session keeps its id.

## The dashboard

- The session details view (`enter`) already shows a "started" line from `JazariLaunchSet` (#122).
  For a session that has no such event (started without jazari), the line shows the project's
  saved set, marked "project set, not confirmed".
- The `c` picker gets a key, `s`, on its last step: "save for this project". The project is that of
  the selected session's folder. The footer hint and README key list name it.

## How this fits with the handoff

The handoff's base set (#122) is the old session's set plus the global ticked defaults. With this
plan, the base set is the union of:

1. the old session's set;
2. the project's set, or the global ticked defaults when the project has none.

The project's set only adds keys. It never drops a key that the old session was using.
`rules::files_for` removes any key that appears twice, and it puts "Working with jazari" first
(#127). The model's extras come after the base set, as before.

## The `c` picker stays

`jazari claude` is one more way to start a session. It does not replace `c`. The `c` picker keeps
every mode that it has: a normal session, an agent team, and "File path only". "File path only"
copies the path of the prompts' file, to paste into a session that is already running.

The selected session's folder can belong to a project with a saved set. Then the picker ticks that
set at the start, in every mode. So `c`, `enter`, `enter` in "File path only" gives a running
session the set of its project. `s` saves the ticked set for that project. `project::set_for` gives
the project and its set for a folder. The picker uses it, and so do `jazari claude` and the handoff.

## Agent teams: `jazari claude --team`

```sh
jazari claude --team                                           # the lead starts with the project's set
jazari claude --team --teammates easy-english,output-hygiene   # the teammates' set for this start only
jazari build project edit --teammates                          # save the teammates' set for the project
```

A project's file has a second list, `teammates`. Every `jazari build project` command takes
`--teammates` to work on that list. Without a saved list, the teammates get the lead's set without
coordination, as `c` does in team mode today.

The lead learns the path of the teammates' file in the same way as with `c`:

1. `rules::ensure_team_in` writes the teammates' file first, then the lead's file.
2. The lead's file ends with an "Agent team" section. That section holds this line, with the path:

   > Before any other work, read `<teammates' file>` and follow it for the rest of your work.

3. The lead starts every spawn prompt with that line.

"Working with jazari" is first in both files. Coordination stays with the lead, so it is never in
the teammates' file.

## The picker in the terminal

`jazari build project edit` and `jazari claude --pick` open a picker in the terminal
(`src/pick_term.rs`). It has the same ticks and keys as `c`: `space` or a number ticks a row, and
`enter` goes to the next step. It has no keys that write, edit or delete prompts. With `--pick`, the
last step offers "Use for this start only" (the default) or "Save for this project", and `s` saves
at once. The `c` picker in the dashboard stays in `src/app.rs`.

## Left out on purpose

- **`--remote`** (control of a session from Telegram). The founder's rule of 2026-11-26 16:29 lets
  Telegram control only jazari's own sandbox work. Opening it to sessions started with
  `jazari claude` comes after the approval socket, as a per-project opt-in. It is not part of this
  plan.
- **A set stored in the repo** (for example `.jazari.json`). That would share a personal choice with
  everyone who clones the repo.

## Parts

1. `src/project.rs` (new): find the project, read and write the set, the `jazari build project`
   commands. The git call and the file system root are passed in, so tests use temp folders and a
   fake git.
2. `src/launch.rs` (new): `jazari claude`. The step that starts `claude` is an injectable function,
   so tests never run `claude`. It reuses `handoff_pick::new_session_id` and the launch-set event
   from `handoff.rs` (moved to a shared place).
3. `src/main.rs`: the `Claude` top-level command. `src/jazari/mod.rs`: the `Project` build command.
   `src/pick_term.rs` (new): the picker in the terminal. `src/rules.rs`: team files with their own
   teammates' set.
4. `src/app.rs`, `src/ui.rs`: the project's set ticked at the start of `c` (also for "File path
   only"), the `s` key in the picker, and the "project set" line. This part comes after #131.
5. `src/handoff.rs`: the base set is the old session's set, then the project's set (or the global
   defaults).
6. Docs: README (the key list, a "Per-project prompts" section, `jazari claude`), CHANGELOG,
   `assets/SKILL.md` (a session can start another session with `jazari claude`).

## Tests

- The project of a subfolder is its repo's main checkout; a worktree gives the same project; a
  folder outside git is itself; two paths to one folder give one project.
- The set falls back to the global defaults when the project has none, and back again after
  `clear`.
- "Working with jazari" is first and once, also when the saved set names it.
- The handoff's base set keeps every key of the old session, adds the project's keys, and holds each
  key once.
- An unknown key is skipped with one warning, and the start goes on.
- `add` refuses an unknown key; `remove` of a key that isn't there says so.
- `jazari claude` passes the args after `--` unchanged, refuses a second `--append-system-prompt-file`
  or `--session-id`, and leaves out `--session-id` with `--resume`.
- A team start writes both files. The lead's file names the teammates' file, and "Working with
  jazari" is first in each file.
- The folder of a project with a saved set gives that set, so the copy-path mode ticks it.
- On Windows, jazari waits for `claude` and passes its exit code on. CI builds this code on Windows,
  but no test runs it, because a test must not start `claude`.
- main.rs's test that parses every documented command stays green.

## Open questions for the review

- Should `jazari claude` without a saved project set and without global defaults start with only
  "Working with jazari", or ask once to save a set? This plan says: start, and print a hint.
- `s` is free in both steps of the `c` picker today (checked in `src/app.rs`: the list uses n, e,
  d, v, u, j, k, x, space and digits; the last step uses j, k, tab and digits). Should "save for this
  project" be the `s` key, or a fourth numbered choice next to session, team and path?
