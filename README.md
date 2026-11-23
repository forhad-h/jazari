# marsad

A live terminal dashboard for every Claude Code session on your machine: what each one is
doing, which ones are waiting on you, which skills they ran, and how many tokens they used.

```
 marsad   3 sessions · 1 active · 2 need you                    today 2.4M · all-time 133.5M 
╭ Sessions ───────────────────────────────────────────────────────────────────────────────────╮
│ SESSION                            PROJECT        CONTEXT LEFT  TIME  TOKENS         STATUS │
│ Fix the flaky login test           marsad         ▰▱▱▱▱ 801.0k   37s    2.0M      APPROVE ● │
│ Pick a database for reports        notes          ▰▱▱▱▱ 920.4k   37s    8.1k     QUESTION ? │
│ Dark mode settings              +2 web            ▰▰▱▱▱ 552.6k   12s  414.8k       ACTIVE ◐ │
╰─────────────────────────────────────────────────────────────────────────────────────────────╯
╭ Dark mode settings · web ───────────────────────────────────────────────────────────────────╮
│now      › add dark mode to the settings page                                                │
│context  ▰▰▰▰▱▱▱▱▱▱ 43%  552.6k left   ·   tokens 414.8k   ·   model claude-opus-5-5         │
│id       1fe612e4-0c3b-4d5e-9a7f-2b8c6d1e0f93  y copies it   ·   ~/Projects/web              │
│agents   main 312.1k · Explore ✓ 15.8k · general-purpose ◐ 1 running 41.3k                   │
╰─────────────────────────────────────────────── enter history  tab subagents  y copy id ─────╯
```

## Install

macOS / Linux:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/forhad-h/marsad/releases/latest/download/marsad-installer.sh | sh
```

Windows (PowerShell):

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://github.com/forhad-h/marsad/releases/latest/download/marsad-installer.ps1 | iex"
```

Or download a binary from [Releases](https://github.com/forhad-h/marsad/releases), or build from
source with `cargo install --git https://github.com/forhad-h/marsad`.

## Usage

```sh
marsad install     # registers hooks + status line in ~/.claude/settings.json (backup saved alongside), creates the folder for your own prompts
marsad install --no-statusline   # hooks only; context size is then estimated from transcripts
marsad             # open the dashboard
marsad uninstall   # removes only marsad's hooks, skill and generated prompt files (your own prompts stay)
marsad update      # install the latest version (shows current → latest and what's new)
```

Restart running Claude Code sessions after `install` so they pick up the hooks.

Keys: `↑↓`/`jk` select · `enter` session history · `tab` subagents · `y` copy the session id · `u` usage history · `D` disk cleanup (`enter` remove, `A` all tier A, `J` ask Claude, `o` auto-clean on/off, `r` rescan) · `p` badges and share to LinkedIn · `P` pin a status window on top of other apps (macOS) · `b` agent board · `c` copy a launch command with your choice of prompts, for a session or an agent team, and write your own (`n` new, `e` edit, `d` delete, `v` versions, `u` undo a delete) · `n` write a new prompt from a rough idea · `a` show/hide ended sessions · `m` alerts on/off · `s` cycle sort column (status → project → context → time → tokens) · `S` reverse sort · `q` quit.
In session history and subagents: `↑↓` scroll · `g`/`G` top/bottom · `tab` switch between them · `esc` back.
In session history also: `f` filter by event kind · `J`/`K` scroll the selected event's details.
History lists the newest events first. By default sessions that are working or waiting on you sit on top and the rest keep their place, so rows don't jump around as sessions run.

### Window on top

`P` opens a small window in the top-right corner that stays above every other app, listing each
live session's status and name with your usage limits, so you can keep an eye on sessions while
reading or browsing without switching back to the terminal. It doesn't take focus when clicked; drag
it anywhere. `P` again or its close button closes it, and it closes with the dashboard. macOS only
(it runs through `osascript`); on other systems use your terminal's own "always on top" setting.

### Alerts

While the dashboard is open it plays a sound when a session starts waiting on you (QUESTION or
APPROVE) and another when a session finishes its work, and shows a desktop notification naming the
project and what it is waiting on. Pressing Esc or interrupting a turn doesn't count as finishing,
and a session that leaves a background shell running finishes only when that work is done. `m`
turns alerts off and on; the choice is kept in `~/.marsad/settings.json`.

macOS uses its Ping and Glass sounds and shows notifications on the lock screen if Script Editor
(which `osascript` runs as) is allowed to in System Settings → Notifications. Linux uses `paplay`
and `notify-send` when installed. Windows plays system sounds only. Without a sound player the
terminal bell rings instead.

### Updates

The dashboard checks GitHub for a new release once a day and shows a yellow
``v0.3.0 available · run `marsad update` `` notice in the bottom right. `marsad update` shows your
current and the latest version, a short "What's new", and installs it with the release's installer
(into the same directory as the running binary). `marsad update --check` only shows the versions.

```sh
marsad update --auto on    # the dashboard installs new versions in the background
marsad update --auto off
```

With automatic updates on, the notice changes to `updated to v0.3.0 · restart marsad`. Set
`MARSAD_NO_UPDATE_CHECK=1` to turn off the daily check. Builds from source (`cargo install`, or a
binary outside a `bin` directory) can't update themselves; reinstall them the same way instead.

### Context and usage limits

The **CONTEXT LEFT** column and the details pane show how many tokens remain before
auto-compact, colored green → yellow (60%) → red (85%). The header shows your 5-hour and 7-day
usage limits with the local time each resets, such as `↻7:20pm` (claude.ai Pro/Max only).

These exact figures come from Claude Code's status line feed, so `marsad install` sets
`statusLine` to `marsad statusline`, which prints a compact `ctx 42% · 412k left · 5h 23% · 7d 41%`.
If you already had a status line, marsad wraps it: your status line keeps rendering exactly as
before, and `marsad uninstall` restores it. Without the status line, the window size is inferred from the
model id (marked `≈`) and usage limits are not shown.

Auto-compact point: 967K on native 1M-context models, otherwise the full window, or your
`autoCompactWindow` / `CLAUDE_CODE_AUTO_COMPACT_WINDOW` if set.

### Handoff warning

When a session's context passes **80%** of the auto-compact point, marsad tells Claude once, on
your next prompt, that the context is nearly full and that it should consider writing a handoff
prompt for a fresh session. `install` registers this as a synchronous `UserPromptSubmit` hook
(`marsad context --hook`, a few ms) so the note reaches the prompt you just sent. The warning comes
back after a compact or `/clear` brings the context below the threshold.

```sh
marsad context              # this session: context used, left, and the warning threshold
marsad context --session 1fe6 --format json
marsad context --warn 70    # warn at 70% instead
marsad context --warn off
```

Inside Claude Code (a Bash tool call or a hook), `marsad context` finds its own session; elsewhere
pass `--session`.

### Disk guard and system meters

A full disk can hang the machine: swap can't grow and Claude Code can't write its transcript.
`install` registers `marsad guard --hook` as a synchronous `PreToolUse` and `UserPromptSubmit` hook
(about 7 ms), so every tool call of every running session checks free space first, whether or not
the dashboard is open. It measures the volume that holds your home folder and the session's folder,
and counts only space a process can write (not APFS purgeable space).

| Level | Default | What happens |
|---|---|---|
| notice | 80% full or under 30 GB free | shown in the meters |
| warning | 90% or 15 GB | Claude is told once to finish the step, start no large builds, pulls or installs, and write a handoff |
| critical | 95% or 8 GB | the next tool call is refused and the session stops, with its state saved; `claude --resume <id>` continues it once space is freed |
| emergency | 98% or 3 GB | the watcher deletes its ballast and interrupts running sessions (SIGINT) |

Each level fires at the percent or the free space, whichever comes first, so large disks aren't
left with 100 GB "full". A warning comes back only after usage drops 2 points below the level. A
stopped session shows `halted: disk almost full` in the dashboard, with its resume command. Change
a level in `~/.marsad/settings.json`, field by field:

```json
{ "disk": { "critical": { "pct": 93, "free_gb": 12 }, "guard": true } }
```

```sh
marsad disk                  # free space on each volume in use, the levels and the guard
marsad disk --format json
marsad disk --guard off      # only warn, never stop a session (--guard on to restore)
marsad system                # disk, memory, swap, CPU and load, with what to be careful about
marsad system --format json  # `ok` and `concerns` for a session deciding whether to start heavy work
```

The dashboard's second header row shows the same disk, memory (with swap) and CPU meters, colored
by how close each is to trouble.

### Background disk watcher

The first session's `SessionStart` hook starts `marsad guard`, a small background process that
checks the same volumes every 20 seconds, so a disk filled between tool calls (a build, an image
pull) is caught too, with or without the dashboard. Only one runs at a time (`~/.marsad/claude/guard.pid`),
and it exits once no session has run for 10 minutes. It needs no launchd or systemd unit.

- **Desktop notification and sound** once per crossing of notice, warning, critical and emergency.
- **Cleanup** of rebuildable data from the warning level (see [Disk cleanup](#disk-cleanup)).
- **Emergency stop**: at the emergency level it sends SIGINT (never a kill) to each running
  session's Claude Code process, once per crossing, after checking the process is still Claude
  Code. The dashboard marks the session `halted`; `claude --resume <id>` continues it. On Windows
  it only notifies.
- **Ballast** (off by default): `disk.ballast_gb` keeps a file of that size at `~/.marsad/ballast`,
  written only while the disk stays under the warning level with it in place. At the emergency
  level it is deleted first, so stopping sessions and cleaning up have room, and written again
  once usage is back under warning.
- **Phone notification** (off by default): from the warning level, once per crossing, to an
  [ntfy](https://ntfy.sh) topic (`disk.ntfy`, the full topic URL) and/or a webhook (`disk.webhook`,
  which gets a JSON POST of `level`, `used_pct`, `free_gb` and `text`). It is sent with `curl`
  and carries only the level, percent full and GB free: no project names or paths.

```json
{ "disk": { "ballast_gb": 5, "ntfy": "https://ntfy.sh/<your-topic>", "webhook": "https://example.com/hook" } }
```

`marsad disk` shows whether the watcher is running, the ballast and which phone notifications are set.

### Disk cleanup

`D` in the dashboard (or `marsad clean`) lists what can be removed, measured on disk, in three tiers.
marsad already knows which projects have live sessions and when each was last active, so "idle"
comes from that rather than file times, which installs and git keep refreshing.

| Tier | What | How it is removed |
|---|---|---|
| **A** rebuilt automatically, no data | npm, npx, Yarn, pip, uv, Go, Gradle and Cargo caches, Homebrew downloads, Xcode DerivedData, Docker build cache; `.next`, `.turbo`, `.parcel-cache` and Cargo `target/` in projects with no session for a day | on its own once the disk reaches the warning level (and at critical even with auto-clean off), at most every 30 minutes, then one notification saying what was freed |
| **B** rebuilt, but slow to get back | `node_modules` in projects idle 14 days, Docker images no container uses, pnpm store, Maven repository, Claude Code worktrees whose branch is merged and clean | after one confirmation |
| **C** holds data | backup copies next to their original (`src.bak-1119`, `app-backup`, `site_old`), Docker volumes no container uses, each container stopped over 7 days (its own files can hold data); worktrees with changes or unmerged commits are listed but never removed | one at a time from `D`, after a confirmation that says it can't be brought back |

`J` in the view (`marsad clean --judge`) asks Claude whether each tier C item can go. It gets
names, sizes, ages and top-level entry names, with the home folder written as `~`, never file
contents, and answers keep, suggest or safe with a reason, shown on the item and in the
confirmation. It only advises; marsad does the removing. The run is isolated like the prompt
writer (`--safe-mode`, no tools, no transcript) and tries Sonnet, then Opus, then your default
model (`models.clean_judge` in the settings changes the first).

What keeps it safe:

- Every removal is one of a fixed set of actions (delete one of a fixed list of folder names,
  `docker builder|image prune`, `docker volume rm <name>`, `docker rm <id>`, `git worktree remove`
  without `--force`). No model writes a command that runs.
- Right before deleting, each folder is checked again: it still exists, it is a plain folder (not
  a link, and no link in its path), it is inside the home or temp folder and at least two levels
  down, it isn't in `.git`, `.ssh` or `.env*`, no session runs in its project, and the tool that
  uses a cache isn't running. Build output removed on its own must be ignored by git or marked by
  Cargo. A backup is removed only while the folder it copies still exists.
- Folders are deleted outright (moving them to the Trash frees nothing) with Rust's standard
  library, never `rm -rf` through a shell.
- Each action is recorded in the `cleanups` table of the usage history, with the bytes it freed;
  `D` and `marsad clean` show the total for the last 30 days. Docker Desktop's disk image may give
  space back only after Docker restarts.

```sh
marsad clean                     # the plan, by tier, with sizes and ids
marsad clean --format json       # the same for a session (`claude` holds Claude's advice)
marsad clean --tier-a            # remove every tier A item now
marsad clean --run npm-cache ~/Projects/web/node_modules   # tier A or B items by id
marsad clean --judge             # ask Claude about the tier C items
marsad clean --auto-clean off    # don't clean tier A on its own at the warning level
```

At the critical level the guard still lets plain `marsad disk`, `marsad system` and `marsad clean`
commands through, so a session can check and free space.
### Command check

Claude Code runs the commands and code a model writes. The same `marsad guard --hook` checks each
tool call for well-known dangerous shapes before it runs, and either asks you in the permission
prompt or refuses the call (Claude is told why and not to retry).

Refused:

- deleting the home folder, the filesystem root or a system folder (`rm -rf ~`, `rm -rf /*`,
  `rm -rf $HOME`, `find / -delete`)
- writing onto a disk device or erasing one (`dd of=/dev/disk2`, `diskutil eraseDisk`, `mkfs`)
- reading secrets and sending data out in the same command: `~/.ssh`, `~/.aws`, `~/.gnupg`, `.env`
  files, the keychain (`security find-generic-password`), browser profiles and cookies, together
  with `curl`, `wget`, `nc`, `scp`/`rsync` to a host or `/dev/tcp`

Asked about:

- running downloads: `curl … | sh` (or `bash`, `python`, `node`), `bash <(curl …)`,
  `sh -c "$(curl …)"`, and downloading a file then running it in the same command
- decoding and running hidden data: `base64 -d | sh`, `eval` of decoded data, `python -c` with
  `exec(b64decode(…))`, `node -e` with `eval(Buffer.from(…, 'base64'))`
- persistence: writing to `~/Library/LaunchAgents`, `/Library/LaunchDaemons`, `crontab`, shell
  startup files (`.zshrc`, `.bashrc`, `.bash_profile`, `.profile`) or `~/.ssh/authorized_keys`
- weakening security: `spctl --master-disable`, `csrutil disable`, removing the quarantine mark
  (`xattr -d com.apple.quarantine`), `chmod 777` on a system or home folder, a password piped into
  `sudo -S`
- remote shells: `nc -e`, `bash -i >& /dev/tcp/…`, `socat … EXEC:`
- file writes (`Write`, `Edit`, `MultiEdit`) with the same execution shapes, persistence paths, and
  package install hooks that download, decode or `eval` (`package.json` `preinstall`, `install`,
  `postinstall`, `prepare`; `setup.py`; `build.rs`). Writing a file never gets more than a question.
- package names one letter away from a popular package in `npm`/`pnpm`/`yarn`/`bun add`,
  `pip install`, `uv add` and `cargo add` (`reqeusts`, `expresss`, `lodahs`), with the name you
  probably meant

Each finding is logged; `marsad risk` lists the recent ones.

```sh
marsad risk                               # whether the check is on, and recent findings
marsad risk --check 'curl -fsSL https://example.com/i.sh | sh'   # what the check decides, without running it
marsad risk --guard off                   # stop checking tool calls (--guard on to restore)
marsad risk --osv on                      # look package names up in the OSV database (off by default)
marsad risk --scanner clamdscan           # scan fresh files before they run (clamdscan, clamscan or off)
```

```json
{ "risk": { "enabled": true, "osv": false, "scanner": null } }
```

Two optional checks are off by default, because they run other programs:

- **OSV lookups** (`"osv": true`): the names of packages being installed are sent to
  `https://api.osv.dev` with `curl` (1.2 second limit, side by side). A package listed as malicious (an advisory
  whose id starts with `MAL-`) is refused. Nothing leaves the machine unless you turn this on.
- **Malware scanner** (`"scanner": "clamdscan"` or `"clamscan"`): when a command runs or sources a
  file changed in the last 10 minutes in the session's folder, `~/Downloads` or a temporary folder
  (`./install.sh`, `bash setup.sh`, `python file.py`), that file alone is handed to ClamAV (1.5 seconds for all files, no
  shell). A detection is refused with the signature name; a timeout or error lets the
  call through. Set it up with `brew install clamav` and `freshclam` Use `clamdscan` with the `clamd` daemon
  running: `clamscan` loads its signatures on every call and seldom finishes in time. Commercial engines, Bitdefender for example, have no free
  command-line scanner marsad can call on macOS.

Limits: the patterns catch the common and the careless cases. Code written to hide what it does,
a script that fetches its payload later, or a command split across several tool calls can get past
them, and a refused command can be reached another way. Claude Code's `/sandbox`, which limits what
commands can read, write and reach on the network, is the stronger layer; this check sits in front
of it, not in place of it.

### Session history

`enter` on a session opens a scrollable timeline of what happened in it, oldest first:

| Event | Shows |
|---|---|
| prompt | what you typed (including `/commands` and `!` shell commands), and the tokens of the turn it started |
| reply | Claude's reply that ended the turn |
| skill | the skill, whether Claude called it (`tool`) or you typed it (`/cmd`), turns, output and ≈ tokens in its window |
| agent | the subagent's type and task, running or done, its tokens, and its final report |
| question | each AskUserQuestion question and your answer (or the note you left when declining) |

Skills and questions from subagents are included, marked with the subagent's type. The pane
under the list shows the selected event in full; `J`/`K` scroll it, and `f` filters the list to
one kind of event. Prompts, replies and reports are kept up to 4,000 characters each.

### Subagents

`tab` on a session lists the main agent and every subagent it launched, oldest first: agent type,
task description, the skill that launched it, skills it ran itself, running or done, tokens used,
and context left before that agent's own auto-compact. Each subagent has its own context window,
so a subagent's used tokens (summed across all its turns) can be far above its context fill.

### Coordinating several sessions

When several Claude Code sessions work in one repo, marsad's coordination rules keep them from editing the same files. Sessions that load the rules share one `AGENT-BOARD.md` at the root of the repo's main checkout (one board for all its worktrees, kept out of git through `.git/info/exclude`; a bare clone or a submodule keeps it inside its git directory). On it they register, claim files before editing them, record binding decisions, message each other, and log conflicts they can't settle as `needs-human`.

`c` opens a picker of prompts: the ones marsad ships, **Coordination** (above) and **Output hygiene** (keeps the request, who asked and how the work was done out of code, commits, PRs and docs), and your own. Add your own as `.md` files in `~/.marsad/prompts/`; the first `# ` heading is its title and the first line of text its summary. Each row is tagged `[built-in]` or `[yours]`. Tick prompts with `space` or their number and press `enter`, then choose what the command starts:

- **Normal session**: one Claude Code session follows the prompts.
- **Agent team**: the lead of an [agent team](https://code.claude.com/docs/en/agent-teams) follows them and passes them on. A teammate never sees the lead's system prompt, only the project's CLAUDE.md, skills and MCP servers and the spawn prompt the lead writes, so marsad writes a second file for the teammates and tells the lead to start every spawn prompt with "read this file". Coordination stays with the lead: teammates coordinate through the team's task list and messages, and the lead claims files on the board for them.

Press `enter` to copy the command. Your choice is remembered, so next time `c`, `enter`, `enter` is enough.

**Writing your own.** `n` on the session list, or in the picker, opens a text box for a rough idea in your own words ("make Claude stop over-explaining in PRs") and an optional name. `tab` moves between the idea, the name and the **Write prompt** and **Cancel** buttons; `enter` writes it (or presses the focused button), `alt+enter` starts a new line, `ctrl+e` opens the idea in `$EDITOR`, and `esc` cancels. Claude turns it into a prompt file: explicit, checkable rules with their reasons, short enough to follow. The review shows the draft, what Claude assumed, and the questions the idea left open. `enter` saves it to your prompts and ticks it, `e` opens it in `$EDITOR`, `r` revises it with a line of feedback, a question's number answers that question (which also revises it), `n` renames it, and `esc` discards it (an unsaved draft asks first: `esc` again discards it). On one of your prompts, `e` opens the same review to edit or revise it and `d` deletes it after asking; `u` brings back the prompt just deleted.

**Versions.** Every edit, rename, restore and delete keeps the text it replaces in `~/.marsad/prompts/.versions/<name>/`, up to the newest 50 per prompt. `v` on one of your prompts lists its earlier versions with when each was replaced and a preview of the text; `enter` makes the highlighted one the prompt again, and the text it replaces is kept as a version too, so a restore can be undone. A built-in prompt can't be changed; `e` on one saves a copy under a new name. Coordination can't be copied, because an agent team recognises it by name to keep it with the lead.

The draft is written by one `claude -p` run with no tools, in `--safe-mode` (no CLAUDE.md, hooks, plugins or MCP servers), from marsad's own folder and without a transcript, so nothing from your projects reaches it and it never shows up as a session. Your situation, names and reasons stay out of the prompt unless they are the rule itself. It uses the strongest model Claude Code accepts, `fable`, falling back to `opus` and then your default when an alias isn't available; set `"generator_model"` in `~/.marsad/settings.json` to choose another. `MARSAD_CLAUDE` points marsad at a different `claude` program.

From the command line:

```sh
marsad prompt new "make Claude stop over-explaining in PRs"   # draft, review, save; --from <file> or stdin also work
marsad prompt new --name "Short PRs" --yes < idea.txt          # save without reviewing; --format json for scripts
marsad prompt edit short-prs                                   # open in $EDITOR; --with "<feedback>" revises it with Claude
marsad prompt copy output-hygiene --as my-hygiene             # a copy of a built-in prompt you can change
marsad prompt rm short-prs                                     # its text is kept as a version
marsad prompt versions short-prs                               # earlier versions; add a number to print one
marsad prompt versions                                         # deleted prompts that can be restored
marsad prompt restore short-prs 2                              # make v2 the prompt again; without a number, the newest
```

Claude Code reads only the last `--append-system-prompt-file` it is given, so the chosen prompts are merged into one file in `~/.marsad/launch/`, written when the command is copied and named after its role, its prompts and a hash of its text, such as `lead-coordination+output-hygiene-8873230f.md`. A session's or lead's file whose command hasn't been copied for a week is removed (a conversation keeps the prompt it started with); a teammates' file is kept, since a resumed lead still points at it. `marsad coordination` prints the stable `~/.marsad/coordination.md`.

The prompts are added to the session's system prompt, so they survive `/clear` and compaction. They apply to new conversations only: a resumed conversation keeps the system prompt it started with. The coordination rules add about 2k tokens to each request of those sessions.

Over SSH, `c` sends the command through your terminal (OSC 52, which kitty, WezTerm, Alacritty and iTerm2 with clipboard access allow; tmux needs `set-clipboard on`) and also shows it so you can copy it by hand. `marsad rules` lists the prompts and `marsad rules coordination output-hygiene` prints the command (`--team` for an agent team, `--format json` for scripts); `marsad coordination` is short for `marsad rules coordination`.

`b` on a session opens its repo's board, read-only and live: agents with their last-seen times, claims, decisions, messages, conflicts and handoff notes. Agents and their claims not seen for more than 2 hours are marked `[stale]`, and `needs-human` conflicts are tagged in yellow. A relation map at the top shows how the agents are linked: messages one sent another (or everyone), conflicts between them, and claims that cover the same file. marsad never edits the board; the sessions own it. To change output hygiene, save a copy (`e` in the picker, or `marsad prompt copy`) and tick that instead.

### Badges and sharing

`p` shows your rank and four achievements, each with its level in words and how far it is to the
next, beside a drawing of your badge and the post to share (see below). The rank counts every token, cache reads included, so it
matches the header's all-time figure.

| Rank | All-time tokens | Badge |
|---|---|---|
| Novice | 0 | carved sandstone |
| Apprentice | 1B | carved cedar |
| Practitioner | 10B | bronze, Moorish tilework |
| Specialist | 40B | silver, Moorish tilework |
| Expert | 100B | gold, Moorish tilework |
| Master | 200B | gold, Persian tilework |
| Grandmaster | 400B | jewelled gold, Persian tilework |

The ladder is paced for years, not weeks: at about 8B tokens a month, a heavy daily user reaches
Practitioner within weeks, Specialist in about five months, Expert in about a year, Master in about
two and Grandmaster in about four. Gold achievements are long-term goals too.

| Achievement | Bronze | Silver | Gold |
|---|---|---|---|
| Longest streak (days in a row with usage) | 14 | 60 | 180 |
| Distinct skills | 25 | 75 | 150 |
| Subagents run | 1,000 | 10,000 | 50,000 |
| Busiest day (tokens) | 2B | 5B | 10B |

marsad keeps the best value each one ever reached, so retention pruning or an import never takes a
badge away. When a rank or level rises, the footer says so once (Esc dismisses it). `marsad badges`
prints the same (`--format json` or `csv` for scripts).

In the share view, `s` saves the badges as an image, `Downloads/marsad-badge-YYYY-MM-DD.png`, and
shows it in your file manager. `marsad badges --png [PATH]` saves it from the command line.
The image is drawn from the view itself (same art, colours and text), so it looks the same in any
terminal.

`p` also prepares a LinkedIn post: a line about today's work drafted by Claude Code, then today's,
the last 7 and 30 days', the daily average and all-time tokens, and your rank. You see the whole
post beside your badges first: `enter` shares it to LinkedIn, `c` copies the whole post (and works
again on the Done screen), `s` saves the badge image, `e` edits the line, `E` opens the whole post
in `$VISUAL`/`$EDITOR`, `r` drafts a new line, `v` shows the next of three variants of the line, and
`q`/`esc` cancels without writing or opening anything. Each variant names the features you worked
on that day (your busiest session titles, with project names taken out), and lines you shared or
copied in the last 10 days are kept in `~/.marsad/share/posts.jsonl` so new drafts don't repeat
them. Confirming:

1. saves a 1200×627 card with your badge, rank, stats, achievements and `git config user.name` to
   `Downloads/marsad-YYYY-MM-DD.png` (or `~/.marsad/cards/`); sharing again the same day
   replaces it,
2. copies the post to your clipboard,
3. opens LinkedIn's post composer with the text filled in,
4. shows the card in your file manager, so you can attach it.

LinkedIn's prefill link is undocumented, so if the composer opens empty, paste the post; a post
whose link would be too long opens the plain feed instead. Over SSH or without a display, steps 3
and 4 are skipped and the link and the card's path are shown.

**What leaves your machine.** Only the drafting does, and only through your own Claude Code login:
one `claude -p` call (Haiku, no tools, no hooks, CLAUDE.md, plugins or MCP, and no transcript, so it
doesn't appear as a session) gets today's session titles, the skills and subagent types you used
with their counts, and the token figures. Every project and folder name from today's sessions is
removed from them first, including the folders above a project outside your home folder (system
folders such as `/srv`, `/opt` or `/Volumes` aside), and project and folder fields are never sent. If the reply still names one,
marsad uses a template line instead and says why. A client or company named only inside a session
title, and not in any project or folder name, is **not** removed: read the line in the preview
before you confirm. Without Claude Code, or if drafting fails or takes over a minute, the post uses
a template line built from the stats, with a note saying so.

```sh
marsad share --print               # print the post and write the card; opens nothing
marsad share --print --no-ai --out card.png
```

## How it works

- **Data** lives in `~/.marsad` (`%USERPROFILE%\.marsad` on Windows; `MARSAD_HOME` moves it). The
  settings, usage history, prompts and launch files are at the top, and Claude Code's own files
  (the event log, status line reports) are in `claude/`. Only the hooks, the `marsad-usage` skill
  and the status line live in `~/.claude`. Versions before 0.11 kept the data in
  `~/.claude/marsad`; marsad copies it over once and leaves that folder as it was, still keeping
  its `coordination.md`, `output-hygiene.md` and `coordination+output-hygiene.md` current for
  saved commands and aliases. Those three will stop being updated in a later release.

- **Status** comes from [Claude Code hooks](https://code.claude.com/docs/en/hooks). Each event runs
  `marsad hook` (async, a few ms), which appends a trimmed line to `~/.marsad/claude/events.jsonl`.

  | Status | Events |
  |---|---|
  | Approve | `PermissionRequest`, `Notification[permission_prompt]` (except for questions) |
  | Question | `AskUserQuestion` tool (including its permission request), `Elicitation`, `Notification[elicitation_dialog]` |
  | Active | `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `SubagentStart`, `PreCompact`; or a background shell Claude started since the session last launched or resumed is still running |
  | Idle | `Stop`, `Notification[idle_prompt]`; or the transcript shows the turn ended with no `Stop` (Esc on a prompt, or an interrupted turn) |
  | Error | `StopFailure` |
  | Ended | `SessionEnd`, or the session's Claude Code process is gone (every hook event records its pid) |

- **Tokens** are read from each session's transcript (`message.usage`, deduped by message id),
  including subagent transcripts under `<session>/subagents/`. Hooks don't carry usage data.
- **Per-skill tokens (≈)** are approximate: after a skill is invoked (Skill tool or a typed
  `/skill`), each assistant turn is charged to it until the next user prompt or the next skill.

### Usage history

`u` opens the Usage view: totals for the period on top (tokens, sessions, average per day, busiest
day, a daily chart, and your all-time total), days on the left, and the selected day's sessions and
skills ranked by tokens on the right. `p` / `P` switch the period (the last 7 days, or everything
kept; with a longer retention also 30, 90 and 365 days). `tab` switches pane, and `enter` on a
session shows its per-day totals and every skill run. "Total" aggregates the whole period.

The history is a SQLite database at `~/.marsad/usage.db`, indexed from Claude Code
transcripts (`~/.claude/projects`). It includes sessions from before marsad was installed.
Tokens are bucketed by the local day each API call happened, so long sessions split across days.

Session details (sessions, per-agent days, skill runs) are kept for the **last 30 days** by
default; `marsad retention` changes that. Rows stay even after Claude Code deletes their
transcripts (it does after `cleanupPeriodDays`, 30 days by default). Each session is indexed when it ends (the `SessionEnd` hook starts a background
`marsad sync`), so it is recorded even if the dashboard isn't open. The dashboard also re-indexes
every minute while open, and `marsad sync` / `report` / `sql` / `total` index on demand (only
changed files are re-read).

Retention never touches the token counter: every session's tokens per agent and day are kept
for good, so the header's today and all-time figures, `report --by day` and the Usage view's daily
chart reach back past the retention window. `marsad total` prints the all-time total, ready to
share, with today, the last 7 and 30 days and their daily averages:

```sh
marsad total                # Claude Code tokens since Sep 21, 2026: 133,533,863 (133.5M) + breakdown
marsad total --format json  # also a `periods` list: today, 7d, 30d, all
marsad retention            # show how many days of details are kept
marsad retention 90         # keep 90 days; older details are deleted on the next sync
marsad retention forever
```

The all-time total starts from the oldest day marsad indexed, so usage that was already gone from
Claude Code's transcripts when marsad first ran isn't included. Days pruned by versions before 0.4
survive only as one sum, so they count toward all time but have no daily rows.

### Export and import

```sh
marsad export                     # writes marsad-export-YYYY-MM-DD.json in the current directory
marsad export ~/marsad-backup.json
marsad import ~/marsad-backup.json
marsad import --restore ~/marsad-backup.json   # this machine's own earlier data, after a reinstall
```

The export holds the token counter, the kept history and your settings. `import` merges it into
this machine's data and prints what it added: days and sessions already present keep the larger
count, so importing the same file twice, or importing a machine's own export back, never double
counts. Settings from the file fill only those not set here.

Use `--restore` when the file came from this same machine (a reinstall, or a new data folder) and
Claude Code's old transcripts are still on disk. Days that a pre-0.4 version had pruned into one
sum are then kept out of the day counter here, so they aren't counted twice. Without it, an
import never removes this machine's own days.

```sh
marsad report --by skill --days 7          # skills by ≈tokens this week
marsad report --by session --date 2026-10-04
marsad report --by day --format json       # also: project, model, agent; csv
marsad sql "SELECT skill, SUM(total) FROM skill_runs GROUP BY skill" --format json
marsad schema
marsad status                              # live: status, activity, context left, usage limits
marsad status --format json --all          # include ended sessions
marsad system --format json                # disk, memory, CPU: room for heavy work?
```

**For Claude:** `marsad install` adds a personal `marsad-usage` skill
(`~/.claude/skills/marsad-usage/SKILL.md`). Ask any Claude Code session things like "which
skills used the most tokens this week?" or "how much context is left in my other sessions?" and
it will use `marsad report` / `marsad sql` / `marsad status`. Before a large build, image pull or
install, it checks `marsad system`. `marsad sql` accepts only read-only
statements.

### Known limitations

- `/btw` side questions and `!` shell commands fire no hook and leave nothing in the transcript, so
  they don't change a session's status.

- After you approve a permission prompt, the row stays on APPROVE until the tool finishes
  (Claude Code fires no hook on approval).
- Sessions started before `marsad install` show up only after they are restarted.
- The event log rotates at 20 MB (`events.1.jsonl` keeps the previous one).

## Development

```sh
cargo test
MARSAD_HOME=/tmp/marsad-dev cargo run          # use a separate event log
```

## Releasing

Releases are built by [`dist`](https://opensource.axo.dev/cargo-dist/) in
`.github/workflows/release.yml` for macOS (arm64, x64), Linux (arm64, x64) and Windows (x64).
Bump `version` in `Cargo.toml`, rename `## Unreleased` in `CHANGELOG.md` to `## X.Y.Z - date`
(dist puts that section in the release notes, and `marsad update` shows it as "What's new"),
commit, then:

```sh
git tag v0.1.0 && git push origin v0.1.0
```

After changing `dist-workspace.toml`, run `dist generate` to refresh the workflow.
