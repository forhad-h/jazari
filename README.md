# jazari

jazari is a small AI software company that runs on your Claude subscription, and a live dashboard
for every Claude Code session on your machine. It has two parts:

- **`jazari build`** builds products. You bring an idea, on the command line or from Telegram. A team of
  AI roles plans it, builds it, checks it, reviews it and merges it, and asks you only for decisions
  and approvals. It learns how you like the work done, and in autonomous mode it keeps improving the
  product on its own.
- **`jazari watch`** watches and protects. It shows what each session is doing, which ones need you,
  which skills they ran and how many tokens they used. It also guards the machine: disk space, memory,
  and risky commands.

`jazari` alone opens the dashboard.

The name comes from Ismail al-Jazari (1136–1206), the engineer whose *Book of Knowledge of Ingenious
Mechanical Devices* describes machines that work on their own.

```
 jazari   3 sessions · 1 active · 2 need you                      today 23.0M · all-time 16.1B 
╭ Sessions ───────────────────────────────────────────────────────────────────────────────────╮
│ SESSION                            PROJECT        CONTEXT LEFT  TIME  TOKENS         STATUS │
│ Fix the flaky login test           jazari         ▰▱▱▱▱ 801.0k   37s    2.0M      APPROVE ● │
│ Pick a database for reports        notes          ▰▱▱▱▱ 920.4k   37s    8.1k     QUESTION ? │
│ Dark mode settings              +2 web            ▰▰▱▱▱ 552.6k   12s  414.8k       ACTIVE ◐ │
╰─────────────────────────────────────────────────────────────────────────────────────────────╯
╭ Dark mode settings · web ───────────────────╮╭ System ──────────────────────────────────────╮
│now      › add dark mode to the settings page││cpu      ▰▱▱▱▱▱▱▱▱▱  10%  7.2 of 8 cores idle │
│id       1fe612e4-0c3b-4d5e-9a7f-2b8c6d1e0f93││memory   ▰▰▰▰▰▱▱▱▱▱  51%  7.8 GiB free        │
│model    claude-opus-5-5                     ││swap     ▰▰▱▱▱▱▱▱▱▱  20%  0.4 of 2.0 GiB used │
│                                             ││disk     ▰▰▰▰▰▱▱▱▱▱  55%  110.9 GB free       │
│                                             ││jazari   cpu 0.4% · mem 38 MB · data 12 MB    │
╰─────────────────────────────────────────────╯╰──────────────────────────────────────────────╯
 ctx 45% · 552.6k left · 5h 23% ↻7:20pm · 7d 41% ↻Sat 9:00am
 ↑↓ select  enter details  tab subagents  y copy name+id  u usage  t/x message/stop  …  q quit
```

## Install

macOS / Linux:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/forhad-h/jazari/releases/latest/download/jazari-installer.sh | sh
```

Windows (PowerShell):

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://github.com/forhad-h/jazari/releases/latest/download/jazari-installer.ps1 | iex"
```

Or download a binary from [Releases](https://github.com/forhad-h/jazari/releases), or build from
source with `cargo install --git https://github.com/forhad-h/jazari`.

`jazari --help` lists the two parts; `jazari watch --help` and `jazari build --help` list their commands.

## Usage

```sh
jazari install     # registers hooks + status line in ~/.claude/settings.json (the first time, your file is backed up alongside), creates the folder for your own prompts, offers to install tmux
jazari install --no-statusline   # hooks only; context size is then estimated from transcripts
jazari             # open the dashboard
jazari uninstall   # removes only jazari's hooks, skill and generated prompt files (your own prompts stay)
jazari update      # install the latest version (shows current → latest and what's new) and add the hooks it brings
jazari watch pause --for 2h   # hold what jazari does on its own; `jazari watch resume` turns it back on
jazari watch actions     # what jazari did, refused or held, and who asked
```

Restart running Claude Code sessions after `install` so they pick up the hooks.

Keys: `↑↓`/`jk` select · `enter` session details · `tab` subagents · `y` copy the session name and id · `t` send the session a message · `x` stop it before its next tool call · `X` interrupt it now · `u` usage history · `D` disk cleanup (`enter` remove, `A` all tier A, `J` ask Claude, `o` auto-clean on/off, `r` rescan) · `p` badges and share to LinkedIn · `P` pin a status window on top of other apps (macOS) · `b` agent board · `c` copy a launch command with your choice of prompts and skills, for a session or an agent team, or only the prompts' file path to paste into a running session, and write your own (`n` new prompt or skill, `e` edit, `d` delete, `v` versions, `u` undo a delete) · `n` write a new prompt from a rough idea · `a` show/hide ended sessions · `m` alerts on/off · `s` cycle sort column (status → project → context → time → tokens) · `S` reverse sort · `q` quit.
The mouse wheel works like `↑↓` in every list and pane; to select text with the mouse, hold `Shift` (`Option` in macOS Terminal and iTerm2) while you drag.
After `kill -9`, the dashboard cannot restore the terminal. Then mouse moves appear in the shell as text such as `35;32;21M`. Run `reset` to fix the terminal.
In session details and subagents: `↑↓` scroll · `g`/`G` top/bottom · `tab` switch between them · `esc` back.
In session details also: `f` filter by event kind · `J`/`K` scroll the selected event's details.
History lists the newest events first. By default sessions that are working or waiting on you sit on top and the rest keep their place, so rows don't jump around as sessions run.

### Window on top

`P` opens a small window in the top-right corner that stays above every other app, listing each
live session in the dashboard's order (name, project, then its status in the dashboard's color)
under the selected session's context and the 5-hour limit, colored as on the dashboard, so you can
keep an eye on sessions while reading or browsing without switching back to the terminal. It
doesn't take focus when clicked; drag it anywhere. `P` again or its close button closes it, and it
closes with the dashboard. macOS only (it runs through `osascript`); on other systems use your
terminal's own "always on top" setting.

### Alerts

While the dashboard is open it plays a sound when a session starts waiting on you (QUESTION or
APPROVE) and another when a session finishes its work, and shows a desktop notification naming the
project and what it is waiting on. Pressing Esc or interrupting a turn doesn't count as finishing,
and a session that leaves a background shell running finishes only when that work is done. `m`
turns alerts off and on; the choice is kept in `~/.jazari/settings.json`.

macOS uses its Ping and Glass sounds and shows notifications on the lock screen if Script Editor
(which `osascript` runs as) is allowed to in System Settings → Notifications. Linux uses `paplay`
and `notify-send` when installed. Windows plays system sounds only. Without a sound player the
terminal bell rings instead.

### Updates

The dashboard checks GitHub for a new release once a day and shows a yellow
``v0.3.0 available · run `jazari update` `` notice in the bottom right. `jazari update` shows your
current and the latest version, a short "What's new", and installs it with the release's installer
(into the same directory as the running binary). `jazari update --check` only shows the versions.

After it installs, the new version updates jazari's hooks in `~/.claude/settings.json`. It adds
the hooks that are new in that version and points the others at the new binary. Your own hooks
and the status line stay as they are. A jazari hook that you removed yourself stays removed:
jazari keeps a list of the hooks it added in `~/.jazari/installed-hooks.json`. Each new session
does the same check, so the hooks also stay current if you never run `jazari update`. After
`jazari uninstall`, nothing is added back. Sessions that are already running use the new hooks
after a restart (or `/hooks reload`).

```sh
jazari update --auto on    # the dashboard installs new versions in the background
jazari update --auto off
```

With automatic updates on, the notice changes to `updated to v0.3.0 · restart jazari`. Set
`JAZARI_NO_UPDATE_CHECK=1` to turn off the daily check. Builds from source (`cargo install`, or a
binary outside a `bin` directory) can't update themselves; reinstall them the same way instead.

### Context and usage limits

The **CONTEXT LEFT** column shows how many tokens remain before auto-compact, colored green →
yellow (60%) → red (85%). The row above the footer reads like Claude Code's own status line: the
selected session's context used and left, then your 5-hour and 7-day usage limits with the local
time each resets, such as `↻7:20pm` (claude.ai Pro/Max only). The header keeps today's and
all-time token totals.

Below the list, the left panel shows what the selected session is doing now, its id and model,
plus any command check finding or waiting message. Press `enter` for its details: the full
context gauge, tokens, permission mode, folder, agents and skills, above its history.

These exact figures come from Claude Code's status line feed, so `jazari install` sets
`statusLine` to `jazari statusline`, which prints a compact `ctx 42% · 412k left · 5h 23% · 7d 41%`.
If you already had a status line, jazari wraps it: your status line keeps rendering exactly as
before, and `jazari uninstall` restores it. Without the status line, the window size is inferred from the
model id (marked `≈`) and usage limits are not shown.

Auto-compact point: 967K on native 1M-context models, otherwise the full window, or your
`autoCompactWindow` / `CLAUDE_CODE_AUTO_COMPACT_WINDOW` if set.

### Handoff warning and `jazari build handoff`

When a session's context passes **80%** of the auto-compact point, jazari tells Claude once that
the context is nearly full and asks it to hand off: write a handoff prompt a fresh session can start
from (the goal, what is done, what is left, decisions, files changed, dead ends, how to check the
work) and pass it to `jazari build handoff`. `install` registers this as a synchronous hook
(`jazari watch context --hook`, a few ms) on `UserPromptSubmit` and `PostToolUse`, so the note also
reaches a long turn between tool calls. The warning comes back after a compact or `/clear` brings
the context below the threshold.

At **90%**, the handoff is required. The guard hook (`jazari guard --hook`) refuses every tool
call except these:

- Reading and searching files (Read, Grep, Glob and similar tools).
- `git status`, `git diff` and `git log`.
- `jazari watch context`, `status`, `system` and `say`.
- The handoff itself: `jazari build handoff <<'EOF'`, with the delimiter in quotes.

The refusal tells Claude what to do next, with the exact commands. Subagents are not blocked,
because their context is their own. If you want the session to keep going, tell it so. It then
runs `jazari watch context --allow <session>`, which asks for your yes first. The allowance lasts
until the next compact or `/clear`.

```sh
jazari watch context              # this session: context used, left, and the warning threshold
jazari watch context --session 1fe6 --format json
jazari watch context --warn 70    # warn at 70% instead
jazari watch context --warn off
jazari watch context --block 95   # require the handoff at 95% instead of 90%
jazari watch context --block off  # never refuse tool calls; only warn
jazari watch context --allow 1fe6 # let this session go past the block until its next compact (asks first)
```

Inside Claude Code (a Bash tool call or a hook), `jazari watch context` finds its own session; elsewhere
pass `--session`.

`jazari build handoff` saves the prompt and opens a new terminal that starts `claude` with it, in the
session's folder. It tries, in order:

1. the terminal the session runs in: a new tmux window when inside tmux, otherwise a new Warp tab
   (through a launch configuration), iTerm2 tab, Terminal.app window, Ghostty, WezTerm, kitty or
   Windows Terminal tab;
2. the system default: Terminal.app on macOS, `x-terminal-emulator` (or GNOME Terminal, Konsole,
   Xfce Terminal, xterm) on Linux, a new console window on Windows;
3. a detached tmux session, and prints the `tmux attach` command.

```sh
jazari build handoff <<'EOF'          # the prompt on stdin (or --file handoff.md)
Goal: ...
EOF
jazari build handoff --terminal tmux  # only this terminal (warp, iterm, terminal, ghostty, wezterm, kitty, windows-terminal, default)
jazari build handoff --keep           # keep this session running (by default it ends 5 s after the new one opens)
jazari build handoff --auto-close off # keep the old session by default (`--close` ends it once)
jazari build handoff --no-launch      # only save it
jazari build handoff list             # saved handoffs, newest first
jazari build handoff resume <id>      # start a saved handoff in this terminal
jazari build handoff resume <id> --no-pick  # start it with the old set and your defaults only
jazari build handoff --pick off       # never let the model add prompts or skills
```

The new session starts with prompts and skills from your library:

1. The prompts and skills of the old session. jazari reads them from the name of the file that the
   old session was started with (`--append-system-prompt-file`). On Windows, or when jazari can't
   read the old session's command line, this part is empty.
2. Your ticked defaults (`jazari build rules`).
3. Up to 3 more that a small model (Haiku, through `claude -p`) picks for the next step in the
   handoff. The model sees each prompt's and skill's key, title and summary, never its text. It can
   only add keys from your library, and it never removes one. If it fails or takes longer than
   20 seconds, the session starts with parts 1 and 2 only.

The new session's first reply lists the set, with the reason for each added key. The details view
of the session in the dashboard (`Enter`) shows it too. Each pick is in `jazari watch actions`.

Closing ends the old Claude Code process; inside tmux it closes the pane. In other terminals the tab
stays open at its shell prompt. A session handed to a detached tmux session is never closed, since
the new one isn't on screen yet. `install` offers to install tmux when it is missing (Homebrew on
macOS, the system package manager on Linux), since it is the last fallback.

### Disk guard and system meters

A full disk can hang the machine: swap can't grow and Claude Code can't write its transcript.
`install` registers `jazari guard --hook` as a synchronous `PreToolUse` and `UserPromptSubmit` hook
(about 7 ms), so every tool call of every running session checks free space first, whether or not
the dashboard is open. It measures the volume that holds your home folder and the session's folder,
and counts only space a process can write (not APFS purgeable space). The System panel and
`jazari watch system` show free space the way Finder does, purgeable space included.

| Level | Default | What happens |
|---|---|---|
| notice | 80% full or under 30 GB free | shown in the meters |
| warning | 90% or 15 GB | Claude is told once to finish the step, start no large builds, pulls or installs, and write a handoff |
| critical | 95% or 8 GB | the next tool call is refused and the session stops, with its state saved; `claude --resume <id>` continues it once space is freed |
| emergency | 98% or 3 GB | the watcher deletes its ballast and interrupts running sessions (SIGINT) |

Each level fires at the percent or the free space, whichever comes first, so large disks aren't
left with 100 GB "full". A warning comes back only after usage drops 2 points below the level. A
stopped session shows `halted: disk almost full` in the dashboard, with its resume command. Change
a level in `~/.jazari/settings.json`, field by field:

```json
{ "disk": { "critical": { "pct": 93, "free_gb": 12 }, "guard": true } }
```

```sh
jazari watch disk                  # free space on each volume in use, the levels and the guard
jazari watch disk --format json
jazari watch disk --guard off      # only warn, never stop a session (--guard on to restore)
jazari watch system                # disk, memory, swap, CPU and load, with what to be careful about
jazari watch system --format json  # `ok` and `concerns` for a session deciding whether to start heavy work
```

The dashboard's bottom-right **System** panel shows the same CPU, memory, swap and disk
meters, colored by how close each is to trouble, and what jazari itself uses. Swap turns
yellow once it passes a quarter of RAM and red past half, however full its own bar is.
CPU shows its idle capacity in cores, such as `7.2 of 8 cores idle`. Once the load average passes
twice the core count, it takes that place in red, for example `load 35.7 · 3.6× cores`;
`jazari watch system` always shows the 1, 5 and 15 minute values.

### Background disk watcher

The first session's `SessionStart` hook starts `jazari guard`, a small background process that
checks the same volumes every 20 seconds, so a disk filled between tool calls (a build, an image
pull) is caught too, with or without the dashboard. Only one runs at a time (`~/.jazari/claude/guard.pid`),
and it exits once no session has run for 10 minutes. It needs no launchd or systemd unit.

- **Desktop notification and sound** once per crossing of notice, warning, critical and emergency.
- **Cleanup** of rebuildable data from the warning level (see [Disk cleanup](#disk-cleanup)).
- **Emergency stop**: at the emergency level it sends SIGINT (never a kill) to each running
  session's Claude Code process, once per crossing, after checking the process is still Claude
  Code. The dashboard marks the session `halted`; `claude --resume <id>` continues it. On Windows
  it only notifies.
- **Ballast** (off by default): `disk.ballast_gb` keeps a file of that size at `~/.jazari/ballast`,
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

`jazari watch disk` shows whether the watcher is running, the ballast and which phone notifications are set.

### Memory and CPU guard

The same hook reads memory, swap and the load average before each tool call and prompt, so a
machine running out of memory or buried in work is caught before it stops responding.

| Level | Default | What happens |
|---|---|---|
| strained | 90% of memory used, or a load over twice the cores | each session is told once to hold off builds, test runs, installs and containers |
| critical | 95% of memory used while swapping, or a load over four times the cores | heavy commands (`cargo build`, `npm install`, `docker build`, `make`, `pytest` and the like) are refused with "run `jazari watch system --wait`, then try again"; light work and the session go on |

`jazari watch system --wait` returns once memory and CPU have room again, or after 110 seconds (under the
Bash tool's two-minute limit) so the session can wait again or carry on with light work. The
background watcher notifies you once each time the level rises. Change the limits under
`resources` in `~/.jazari/settings.json`, or set `"guard": false` there to only warn:

```json
{ "resources": { "memory_strained_pct": 85, "memory_critical_pct": 95, "load_strained_per_core": 2, "load_critical_per_core": 4 } }
```

### Talking to sessions

jazari can reach a running session even when you aren't at its terminal, and a session can reach
you through jazari.

- **`t`** (or `jazari watch tell <id> "…"`) sends the selected session a message. The guard hook hands it
  to Claude at the session's next tool call or prompt, as a note to act on before going on. If the
  session is idle, jazari also sends it a wake-up through Claude Code's session socket (macOS and
  Linux). The wake-up starts a turn, unless the session holds it, and carries none of the message
  text. The details pane
  shows messages still waiting.
- **`x`** (`jazari watch tell <id> --stop`) stops the session before its next tool call. The turn ends
  cleanly and `claude --resume <id>` continues it. A prompt you send the session first cancels it.
- **`X`** (`jazari watch tell <id> --interrupt`) interrupts it now, as Esc does, after checking the process is
  still that session's Claude Code. Both ask for `y` first.
- **`jazari watch say "…"`**, run by the session, shows what it is doing on its row in the dashboard;
  `--attention` also marks it as needing you, with the question sound and a notification.

An idle session picks its messages up with your next prompt to it: hooks run only while a session
works. Start sessions with the **Working with jazari** prompt (`c`, or `jazari build rules jazari`) so
Claude knows to read jazari's notes, post its progress with `jazari watch say`, wait for resources with
`jazari watch system --wait`, and stop when asked.

### Telegram

`jazari build` connects jazari to a Telegram bot, so you can check on sessions and steer them
from your phone. It is off until you set it up, and nothing is sent anywhere before that.

1. Create a bot with [@BotFather](https://t.me/BotFather) (`/newbot`), and send it `/setjoingroups`
   → Disable so it can't be added to groups.
2. Save the bot token where jazari reads it, without it passing through a command line or a chat.
   - On macOS, use the Keychain: `security add-generic-password -a "$USER" -s jazari-telegram-token -w`.
   - Elsewhere, set `JAZARI_TELEGRAM_TOKEN`.
3. Pair your chat: `jazari build telegram pair` shows a one-time code of 10 letters and digits,
   such as `ABCDE-FGHJK`. Send that code to the bot in a private chat within 10 minutes. Case,
   spaces and the dash do not matter. Each chat gets one try. Messages that are not shaped like a
   code, such as `/start`, do not count. After a wrong code, the bot ignores that chat until you
   pair again, and the terminal tells you. From then on, the bot obeys
   only the paired chat. Every other chat is ignored and gets no reply.
4. Start the bridge: `jazari build bridge` keeps running until you stop it, so run it in tmux or a
   spare terminal. On macOS, `jazari build service install` runs it in the background instead (see
   Background services).

#### Background services

On macOS, jazari can run the bridge and the supervisor as background services. A service starts
when you log in. If it stops with an error, macOS starts it again after 60 seconds. Nothing runs
this way until you install it.

```sh
jazari build service install               # the bridge
jazari build service install supervisor    # the supervisor too; it spends your Claude usage
jazari build service status                # installed? running? where is the log?
jazari build service uninstall             # stop and remove both; the logs stay
```

- Each service is a LaunchAgent in `~/Library/LaunchAgents` (`jazari.bridge.plist`,
  `jazari.supervisor.plist`).
- Its output goes to `~/.jazari/logs/bridge.log` or `supervisor.log`.
- The service gets the `PATH` of the shell that installed it, so it finds `docker` and `git`. After
  you change your `PATH`, run `install` again.
- `jazari update`, and the dashboard's automatic update, start the installed services again on the
  new version. A supervisor in the middle of a task finishes it first, then restarts by itself.
  `jazari watch pause` holds these restarts, and `jazari watch actions` lists them.
- Stop any bridge you started by hand first. Only one bridge can run at a time. If another copy
  is still running, the service keeps stopping and starting again every minute, and
  `service status` tells you.
- Other systems are not supported yet. There, run `jazari build bridge` and `jazari build run` in
  tmux or a terminal that stays open.

In the paired chat:

| Command | What it does |
|---|---|
| `/status` | Live sessions, which ones need you, and the 5-hour and 7-day usage |
| `/tell <id> <message>` | A message for a session; give at least 4 letters of its id |
| `/stop <id>` | Stops a session before its next tool call |
| `/pause [30m\|2h\|1d]` and `/resume` | The same as `jazari watch pause` and `jazari watch resume` |
| `/new <name> <idea>`, `/idea <product> <idea>` | Start a product, or give one a new idea (see Building products) |
| `/tasks` | What the team is doing, and what waits for you |
| `/answer <task> <answer>` | Answer a task's question |
| `/approve <task>`, `/deny <task>` | Decide on a deploy, spend or contact; the buttons do the same |

When a session runs `jazari watch say --attention "…"`, the bridge sends the note to the chat.
`jazari build notify "…"` sends your own message, from a script or from stdin. `jazari watch actions`
lists everything done from Telegram with the origin `telegram`.

Before anything is sent, credentials are replaced with `[redacted]`:
- known token prefixes (`sk-ant-`, `ghp_`, `github_pat_` and others);
- Telegram bot tokens;
- the value in `KEY=value` when the key names a token, secret or password.

`jazari build telegram status` checks the setup, `telegram test` sends a test message, and
`telegram unpair` forgets the chat.

### Building products

jazari runs a small AI software company on your Claude subscription. You bring the ideas, and it plans, builds,
reviews and merges them. It asks you only for decisions and approvals.

```sh
jazari build new notes "A private, end-to-end encrypted notes app"   # a new repo, planned by the Chief of Staff
jazari build idea notes "Add sharing a note by link"                 # a new idea for an existing product
jazari build product add shop ~/code/shop                            # or bring a repo you already have
jazari build task shop "Add rate limiting to login" --role backend-engineer --priority 2
jazari build plan max5                                               # pro, max5 or max20: how hard it works
jazari build run                                                     # the supervisor; --once runs at most one task
jazari build service install supervisor                              # macOS: run it in the background, from login
jazari build tasks                                                   # the queue, and what waits for you
jazari build answer 12 "Use Twilio"                                  # answer a task's question
jazari build approve 14                                              # decide on a deploy, spend or contact
jazari build deny 14                                                 # or deny it
jazari build personas                                                # the roles workers take
jazari build mode notes autonomous                                   # keep improving it after your tasks (or: normal)
jazari build feedback notes "Keep the screen text short"             # how you like the work; learned after a few times
jazari build prefer all design "Primary buttons are green"           # a preference that applies at once
jazari build prefs                                                   # what jazari learned about you
jazari build forget 4                                                # drop rule #4 (it stays in the history)
```

**Before the first run:** save a long-lived Claude login token for the workers. Run `claude setup-token`,
then on macOS save the token with `security add-generic-password -a "$USER" -s jazari-claude-token -w`.
Elsewhere, set `JAZARI_CLAUDE_TOKEN`. Docker must be running. New products' repos go in
`~/.jazari/build/products/` (under `JAZARI_HOME` when it is set), or in the folder set by `jazari.products_dir`
in `settings.json`.

**The company.** Each task names a role from `jazari build personas`. There are 36 roles:
- **Leadership and planning:** Chief of Staff, Product Manager, Architect, Security Architect, CISO, Tech Lead.
- **Engineering:** backend, frontend, mobile, data, ML, edge/offline, platform/SRE, database.
- **Design:** UX, UI, accessibility, technical writing.
- **Quality:** QA, bug hunter, performance, code review.
- **Security:** AppSec, red team, blue team, supply chain, cryptography, privacy, compliance, incident response.
- **Growth:** market, research, growth, communications, support.

Planning roles use the plan's lead model (Opus on Max), building roles use Sonnet, and support uses Haiku. A new idea
goes to the Chief of Staff. It writes `company/VISION.md`, the roadmap, an architecture ADR, the threat model and
`SECURITY.md`, and then queues the first milestone's tasks. They run in order: each starts once the one before it
is merged. These files in the product's repo are the team's memory.

**The pipeline: nothing a worker claims is trusted until it is verified.** Every task runs as a fresh
headless Claude session, in the product's container, inside a fresh clone that jazari makes in
`~/.jazari/build/work/<product>/`. The container never sees your repo or its `.git`, and your checkout is
never switched. The worker's commits come back as a git bundle, which jazari fetches into your repo as
`jazari/task-<id>`. Every git command jazari runs on your Mac turns off hooks and fsmonitor, so nothing a
worker writes can run there. A branch with new commits passes these gates in order:

1. **Harness checks.** These are deterministic and cost no tokens:
   - jazari runs `company/checks.sh` (build, tests, linters, type checks) in the container, on a fresh clone of
     the branch. The script comes from `main`, not from the branch, so a worker can't weaken the gate it is
     judged by. Until `main` has a script, the branch's new one is used once; jazari tells you, and that
     change gets full code and security review;
   - it scans the diff for credentials, including key files, tokens, JWTs and passwords in URLs.

   Code without a checks script fails this gate. The commit that passes is pinned: every later stage judges
   it, and the merge uses exactly that commit.
2. **The evaluator.** A separate, read-only judge with a fresh context scores the work against the builder's role
   rubric (`assets/jazari/rubrics.md`):
   - every role is judged on: done as asked, claims match evidence, scope, and no secrets;
   - each kind of work adds its own criteria: code, plan, design, docs or security.

   Each criterion is pass or fail, with evidence. The harness's check output is given to the judge as evidence,
   so a claim like "the tests pass" is verified, not believed. To pass:
   - every critical criterion must pass;
   - at least 80% of all criteria must pass;
   - the overall score must be 4 or 5.
3. **Code review, then security review.** Both reviewers are read-only. Changes to `*.md` files only skip both
   reviews; any other change, `company/checks.sh` included, gets both.
4. **The merge queue.** The merge commit is built aside, `company/checks.sh` runs again on the merged result, and
   only then does `main` move.

A failed gate sends a fix task back to the role that built the branch. The fix carries the findings, the failed
criteria and the original spec. After three rounds the task waits for you. A merge that conflicts goes back to
the builder. If your checkout of `main` has uncommitted changes, the merge waits until it is clean. jazari doesn't
push.

**Quality per role.** Every judgment is recorded. `jazari build quality` shows, for each role and gate:
- how often it ran;
- how often it passed;
- the average score.

A building role that fails most of its last 8 evaluations moves up to the plan's lead model, and jazari tells you.

**Changing the team.** `jazari build personas --export` copies the built-in `personas.md` and `rubrics.md` to
`~/.jazari/build/`. jazari uses your copies from then on. `jazari build evals` checks a change with effort
matched to its size, so a small edit costs little or nothing:

- **Nothing changed:** each role, each rubric group and the evaluator have a fingerprint. If none changed, no
  tokens are spent.
- **Lint, always run, free:** duplicate or missing roles, unknown rubric groups, rubrics with no critical
  criterion, unsafe instructions (such as "git push" or "always approve") and the prompt-size budget.
- **A minor edit (a few words):** lint only, then the canary watches live results.
- **A moderate edit:** the judges see only the known-bad calibration cases (`assets/jazari/evals.json`), in one
  batched call per rubric group, on the small model at low effort.
- **A major change:** every case, on the production judge model at high effort. A change counts as major when a
  criterion is added, removed or re-weighted, a role's tier or rubric changes, more than a third of the text
  changes, or it is the first check.

A judge that approves one of the planted defects blocks `jazari build run`. Such defects include missing tests
behind a "tests pass" claim, SQL injection, scope creep, a committed key, a vague plan, poor contrast, a wrong
command in docs or an unproven security finding. `--skip-evals` overrides the block. Live judgments are tagged
with the role's prompt version, and if a changed role passes clearly less often than its earlier versions, jazari
tells you.

**Two modes, set per product.**
- **Normal (the default):** jazari does the tasks from your ideas and answers, then stops.
- **Autonomous:** when your tasks are done, jazari keeps improving the product. The jobs are a bug hunt, a dependency
  vulnerability check, a red-team attack on the local app, a usability and accessibility audit, a performance check,
  a docs refresh, and finding the next improvement. Each job runs only when it is due and the code changed since it
  last ran, so an idle product costs no tokens. Cheaper plans run the jobs less often: for example, the bug hunt runs
  weekly on Pro, every 2 days on Max 5x and daily on Max 20x.

Use `jazari build new <name> <idea> --autonomous`, `jazari build mode <product> autonomous|normal`, or
`/mode` in Telegram.

**What needs you.** Every message jazari sends you is a short card in Easy English:
1. an icon and a title;
2. what happened or what is needed;
3. why;
4. the options, with what each one changes;
5. the recommendation;
6. how to reply.

Options come as buttons. A worker that needs a decision asks this way, and you tap a button or reply with
`/answer`. To deploy, spend money or contact real users, a worker must ask for approval with ✅ Approve and ❌ Deny
buttons. No production, payment or outreach credentials ever enter the container. Approved work comes back as
runbooks for you to run.

**It pushes back.** Workers check your instructions against the threat model, the security rules, the ADRs and user
data. When an instruction conflicts with them, they ask instead of following it silently. They offer a safer
alternative with the evidence. You decide, and a choice against the advice is recorded as an ADR with your reason.
A few choices cross a hard line: shipping a known critical vulnerability, a secret in the code, harm to user data,
or anything illegal. For those, a button isn't enough; you reply with `/answer <task> override: <your reason>`.

**It learns how you like the work.** Everything you tell jazari is a signal: answers, ideas, `/feedback`, and plain
messages to the bot. A small batched call turns the signals into short rules, such as "Primary buttons are green".
- **Learning:** a rule becomes active after you say it 3 times, or 2 times when you are firm ("always", "never").
  `/prefer` sets one at once.
- **Using:** each role gets only the rules for its area (a designer gets design rules, a writer gets copy rules), and
  the evaluator checks them, so you don't have to ask a fourth time.
- **Changing:** when you go in a new direction twice, the new rule replaces the old one, and the old one stays in the
  history.
- **Fading:** a rule you haven't mentioned in 60 days becomes a suggestion, which is followed but not checked.
- **Refusing:** a request that would weaken quality or security, such as "skip the tests", is never learned as a
  rule.
- **Telling you:** changes are not pushed to Telegram. `/prefs` lists them, each with `/forget <id>` to undo it.
  The whole profile is in `~/.jazari/build/FOUNDER.md`.

**It learns from its own rework.** When a branch needed fixes before it merged, jazari draws a one-line lesson
from what the gates found. The next workers get it, and every 5 lessons a task writes them into the product's
`company/LEARNINGS/`. When the same review criterion fails 3 times in two weeks, jazari proposes an automatic check
in `company/checks.sh`, which is the cheapest lasting fix. You accept it with one tap.

**Skills.** Workers get jazari's skills in their Claude folder. The default is Easy English, with its checker.
`"jazari": {"skills": ["easy-english", "ae-bangla-copy"]}` in `settings.json` picks others, and `["all"]` gives
every skill in the library. The checker's report on changed docs goes to the evaluator as evidence.

**The container.** The first task builds the worker image (`jazari-worker:2`: Node.js, Python 3, a C toolchain, git, ripgrep and shellcheck; workers install anything else in user space) and creates the product's container
(`jazari-<name>`). The container:
- sees only the product's work folder (the clones jazari makes), mounted at `/work`;
- runs as a normal user, with no Linux capabilities;
- is limited to 2 CPUs, 2 GB of memory and 512 processes;
- is restarted before every task and every check run, so nothing a worker starts outlives it.

Workers run with permissions bypassed, which applies only inside that container. Only the Claude runs get the
login token; the checks and git commands don't. A run is stopped after 45 minutes.

**Limits to know.** These are not handled yet; the plan to fix them is in
`docs/plans/2026-11-26-jazari-hardening-followups.md`:
- **Open network.** The container can reach the internet. Asking before a deploy, a spend or contacting users
  is a rule the workers follow, not something the container enforces. A worker that ignores the rule could
  still use the network on its own.
- **The audit record's check stays on your Mac.** Someone with access to your Mac can rewrite the record and its
  saved head together, and `jazari build audit` won't notice. A copy that this machine can't change would fix
  that, and it is planned.
- **Stopping during a task.** Ctrl-C, or macOS stopping the service, lets the supervisor finish the task it is on.
  Then it saves the audit head and exits. A second Ctrl-C stops it at once. macOS waits only about 20 seconds,
  so it cuts off a long task. Then the head is not saved, and the task goes back in the queue at the next start.
- **The hard-line flag comes from the worker.** A worker marks a question as a hard line, so the written override
  is a courtesy check, not a gate. The gates still stop the real risks: the checks, the reviews, and no production,
  payment or outreach credentials in the container.

**Usage limits.** Each run reports the 5-hour and 7-day windows, and jazari waits when it must.
- When the next task would eat into the share kept for you, jazari sleeps until that window resets. The share is
  30%/25% on Pro, 15%/15% on Max 5x and 10%/10% on Max 20x.
- It also sleeps if a window refused the run.
- It sleeps when it is ahead of an even pace through the week.
- A task refused for usage goes back in the queue without counting as a failure.
- `jazari build limits` shows the last reading.

**Telling you.** When Telegram is paired, jazari sends you a message only when you must decide or act:
- a question, or a deploy, spend or contact to approve;
- a task that keeps failing, or that is set aside until you say how to go on;
- a proposal to add a check, or a role that got worse after a change;
- a product on hold, or a supervisor that won't start or keeps failing;
- an audit record that was changed;
- a session that asks for you with `--attention`.

News (built, merged, sleeping until a limit resets, preferences learned) is not sent. Ask with `/status`,
`/tasks` or `/prefs`, and the supervisor's output has all of it.

**Pausing.** `jazari watch pause` (or `/pause`) pauses it.

**The record.** Every start, finish, merge and sleep is added to `~/.jazari/build/audit.jsonl`. Each entry
carries the hash of the one before it, so `jazari build audit` finds an edited or deleted entry.
- The head is the hash of the last entry. The supervisor saves the entry count and the head in
  `audit-head.json` when it starts, when it stops and every 50 entries.
- `jazari build audit` fails when the record has fewer entries than that copy, or a different hash at that
  entry. So it finds entries cut from the end, and a record that was written again. From 50 entries on, a
  missing copy is an error too.
- If the record is already broken when the supervisor starts, it tells you on Telegram.

### Disk cleanup

`D` in the dashboard (or `jazari watch clean`) lists what can be removed, measured on disk, in three tiers.
jazari already knows which projects have live sessions and when each was last active, so "idle"
comes from that rather than file times, which installs and git keep refreshing.

| Tier | What | How it is removed |
|---|---|---|
| **A** rebuilt automatically, no data | npm, npx, Yarn, pip, uv, Go, Gradle and Cargo caches, Homebrew downloads, Xcode DerivedData, Docker build cache; `.next`, `.turbo`, `.parcel-cache` and Cargo `target/` in projects with no session for a day | on its own once the disk reaches the warning level (and at critical even with auto-clean off), at most every 30 minutes, then one notification saying what was freed |
| **B** rebuilt, but slow to get back | `node_modules` in projects idle 14 days, Docker images no container uses, pnpm store, Maven repository, Claude Code worktrees whose branch is merged and clean | after one confirmation |
| **C** holds data | backup copies next to their original (`src.bak-1119`, `app-backup`, `site_old`), Docker volumes no container uses, each container stopped over 7 days (its own files can hold data); worktrees with changes or unmerged commits are listed but never removed | one at a time from `D`, after a confirmation that says it can't be brought back |

`J` in the view (`jazari watch clean --judge`) asks Claude whether each tier C item can go. It gets
names, sizes, ages and top-level entry names, with the home folder written as `~`, never file
contents, and answers keep, suggest or safe with a reason, shown on the item and in the
confirmation. It only advises; jazari does the removing. The run is isolated like the prompt
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
  `D` and `jazari watch clean` show the total for the last 30 days. Docker Desktop's disk image may give
  space back only after Docker restarts.

```sh
jazari watch clean                     # the plan, by tier, with sizes and ids
jazari watch clean --format json       # the same for a session (`claude` holds Claude's advice)
jazari watch clean --tier-a            # remove every tier A item now
jazari watch clean --run npm-cache ~/Projects/web/node_modules   # tier A or B items by id
jazari watch clean --judge             # ask Claude about the tier C items
jazari watch clean --auto-clean off    # don't clean tier A on its own at the warning level
```

At the critical level the guard still lets plain `jazari watch disk`, `jazari watch system` and `jazari watch clean`
commands through, so a session can check and free space.
### Command check

Claude Code runs the commands and code a model writes. The same `jazari guard --hook` checks each
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

Each finding is logged; `jazari watch risk` lists the recent ones.

```sh
jazari watch risk                               # whether the check is on, and recent findings
jazari watch risk --check 'curl -fsSL https://example.com/i.sh | sh'   # what the check decides, without running it
jazari watch risk --guard off                   # stop checking tool calls (--guard on to restore)
jazari watch risk --osv on                      # look package names up in the OSV database (off by default)
jazari watch risk --scanner clamdscan           # scan fresh files before they run (clamdscan, clamscan or off)
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
  command-line scanner jazari can call on macOS.

Limits: the patterns catch the common and the careless cases. Code written to hide what it does,
a script that fetches its payload later, or a command split across several tool calls can get past
them, and a refused command can be reached another way. Claude Code's `/sandbox`, which limits what
commands can read, write and reach on the network, is the stronger layer; this check sits in front
of it, not in place of it.

### Safety net for jazari's own actions

jazari changes things outside its own reports: it messages, stops, interrupts and ends sessions,
removes caches, changes settings and updates itself. Every one of those goes through one gate:

- **Who asked** is recorded with each action: you in the dashboard or a terminal, a Claude Code
  session (by its id), the background watcher, or a hook. A message one session sends another
  reaches it marked as not from you, so Claude weighs it instead of following it.
- **A session asks you first.** Before a session runs a jazari command that stops, interrupts or
  ends a session, removes cleanup items with `--run`, changes a guard or setting, pauses jazari,
  updates or installs it, the hook shows Claude Code's permission prompt with the reason. Edits to
  jazari's data folder by a session ask too. Reading status, `say`, `tell` with a message, a
  handoff of its own session and `jazari watch clean --tier-a` run without asking.
- **No double fire.** The same interrupt can't hit a session twice within 30 seconds (a second
  Ctrl-C quits Claude Code), nor the same end-session within a minute, even from two dashboards.
- **Checked at the last moment.** Before an interrupt or end, the process must still be Claude
  Code and older than the session's run; `node` counts only when it runs Claude Code. Ending a
  session after a handoff reads its process from jazari's records, never from arguments, and
  closes a tmux pane only when the pane holds that session. A session can only end itself.
- **Limits on messages.** Control characters are dropped, a message is cut at 2,000 characters,
  and no more than 20 wait for one session.
- **One cleanup at a time.** Cleanups from the dashboard, the command line and the automatic one
  share a lock.
- **`jazari watch pause`** (`--for 30m`, `2h`, `1d`) holds everything jazari does on its own:
  interrupting sessions at the disk emergency, ending a session after a handoff, automatic
  cleanups and automatic updates. Warnings, the command check and what you ask for still work.
- **`JAZARI_DRY_RUN=1`** makes every action report what it would do and do nothing.
- **`jazari watch actions`** (`-n 50`, `--format json`) lists what jazari did, refused, held or
  dry-ran, with who asked and why, from `actions.jsonl` in jazari's data folder.

`jazari watch disk --guard off` now also stops the background watcher from interrupting sessions at the
emergency level, as it says; it only warns.

### Session details

`enter` on a session opens its details: what it is doing now, context, tokens, model, permission
mode, folder, agents by type and skills, above a scrollable timeline of what happened in it,
oldest first:

| Event | Shows |
|---|---|
| prompt | what you typed (including `/commands` and `!` shell commands), and the tokens of the turn it started |
| reply | Claude's reply that ended the turn |
| skill | the skill, whether Claude called it (`tool`) or you typed it (`/cmd`), turns, output and ≈ tokens in its window |
| agent | the subagent's type and task, running or done, the model it ran on (or the model the launch asked for, until it replies), its tokens, and its final report |
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

When several Claude Code sessions work in one repo, jazari's coordination rules keep them from editing the same files. Sessions that load the rules share one `AGENT-BOARD.md` at the root of the repo's main checkout (one board for all its worktrees, kept out of git through `.git/info/exclude`; a bare clone or a submodule keeps it inside its git directory). On it they register, claim files before editing them, record binding decisions, message each other, and log conflicts they can't settle as `needs-human`.

`c` opens a picker of prompts: the ones jazari ships, **Coordination** (above) and **Output hygiene** (keeps the request, who asked and how the work was done out of code, commits, PRs and docs), and your own. Add your own as `.md` files in `~/.jazari/prompts/`; the first `# ` heading is its title and the first line of text its summary. Each row is tagged `[built-in]` or `[yours]`. Tick prompts with `space` or their number and press `enter`, then choose what the command starts:

- **Normal session**: one Claude Code session follows the prompts.
- **Agent team**: the lead of an [agent team](https://code.claude.com/docs/en/agent-teams) follows them and passes them on. A teammate never sees the lead's system prompt, only the project's CLAUDE.md, skills and MCP servers and the spawn prompt the lead writes, so jazari writes a second file for the teammates and tells the lead to start every spawn prompt with "read this file". Coordination stays with the lead: teammates coordinate through the team's task list and messages, and the lead claims files on the board for them.

Press `enter` to copy the command. Your choice is remembered, so next time `c`, `enter`, `enter` is enough.

**Writing your own.** `n` on the session list, or in the picker, opens a text box for a rough idea in your own words ("make Claude stop over-explaining in PRs") and an optional name. `tab` moves between the idea, the name and the **Write prompt** and **Cancel** buttons; `enter` writes it (or presses the focused button), `alt+enter` starts a new line, `ctrl+e` opens the idea in `$EDITOR`, and `esc` cancels. Claude turns it into a prompt file: explicit, checkable rules with their reasons, short enough to follow. The review shows the draft, what Claude assumed, and the questions the idea left open. `enter` saves it to your prompts and ticks it, `e` opens it in `$EDITOR`, `r` opens a box under the draft where you ask Claude for a change in your own words (`alt+enter` for a new line, `ctrl+e` for `$EDITOR`, `enter` sends it, `esc` goes back to the draft), a question's number answers that question in the same box (which also revises it), `n` renames it, and `esc` discards it (an unsaved draft asks first: `esc` again discards it). On one of your prompts, `e` opens the same review with that box already open, so you can type the change and press `enter`; Claude rewrites the prompt with the same model that writes new ones, and you review the result before it is saved. `d` deletes one of your prompts after asking; `u` brings back the prompt just deleted.

**Versions.** Every edit, rename, restore and delete keeps the text it replaces in `~/.jazari/prompts/.versions/<name>/`, up to the newest 50 per prompt. `v` on one of your prompts lists its earlier versions with when each was replaced and a preview of the text; `enter` makes the highlighted one the prompt again, and the text it replaces is kept as a version too, so a restore can be undone. A built-in prompt can't be changed; `e` on one saves a copy under a new name. Coordination can't be copied, because an agent team recognises it by name to keep it with the lead.

The draft is written by one `claude -p` run with no tools, in `--safe-mode` (no CLAUDE.md, hooks, plugins or MCP servers), from jazari's own folder and without a transcript, so nothing from your projects reaches it and it never shows up as a session. Your situation, names and reasons stay out of the prompt unless they are the rule itself. It uses the strongest model Claude Code accepts, `fable`, falling back to `opus` and then your default when an alias isn't available; set `"generator_model"` in `~/.jazari/settings.json` to choose another. `JAZARI_CLAUDE` points jazari at a different `claude` program.

From the command line:

```sh
jazari build prompt new "make Claude stop over-explaining in PRs"   # draft, review, save; --from <file> or stdin also work
jazari build prompt new --name "Short PRs" --yes < idea.txt          # save without reviewing; --format json for scripts
jazari build prompt edit short-prs                                   # open in $EDITOR; --with "<feedback>" revises it with Claude
jazari build prompt copy output-hygiene --as my-hygiene             # a copy of a built-in prompt you can change
jazari build prompt rm short-prs                                     # its text is kept as a version
jazari build prompt versions short-prs                               # earlier versions; add a number to print one
jazari build prompt versions                                         # deleted prompts that can be restored
jazari build prompt restore short-prs 2                              # make v2 the prompt again; without a number, the newest
```

### Skills

The picker lists the prompts under a **Prompts** heading, then the skills under a **Skills** heading. Each skill shows the first sentence of its description, and the highlighted one shows the whole description, which says when Claude loads it. A ticked skill goes into the launch file as a short section: the session must load each skill with the Skill tool before the first step its description names, and follow it for the whole session. An agent team's teammates' file gets the same section. jazari remembers the ticked skills with the prompts.

jazari ships one skill, **Easy English** (`easy-english`): Claude writes technical English that every reader understands on the first read, whatever their first language, in commit messages, pull requests, comments, docs, UI text and long replies. It comes with a checker (`scripts/check.py`, Python 3 with no packages) that finds long sentences, idioms, noun stacks and words outside a B2 word list. Without Python 3 the skill skips the checker and applies its rules by hand.

Claude Code loads skills from `~/.claude/skills/`, so when you copy a command with a skill ticked, jazari makes sure the skill is there:

- No copy there: jazari installs its library copy, with a `.jazari-skill.json` file that records which files it wrote.
- A copy jazari installed that nobody changed since: jazari updates it to the library's version.
- Any other copy (one you wrote, or one jazari installed that you changed by hand): jazari uses it as it is, unless the library's skill has a newer `version:` in its frontmatter than the copy (a copy with no version counts as older). Then jazari keeps the old copy in `~/.jazari/skill-backups/<name>/` and installs the new version.

The footer says which of these happened. The same check runs at the start of every session: a ticked skill that is missing from `~/.claude/skills` is installed again, and installed copies of library skills, there and in the project's `.claude/skills` folders, are brought up to date. Claude Code picks up a skill installed this way a moment after the session starts, by its next turn at the latest; the launch prompt tells Claude to run `jazari build skill sync` if a skill it needs is still missing. `JAZARI_CLAUDE_SKILLS` points jazari at another skills folder, for testing.

**Your own skills** are folders in `~/.jazari/skills/<name>/`, each with a `SKILL.md`, tagged `[yours]`. `n` on a skill in the picker writes a new skill from a rough idea (on a prompt, it writes a new prompt), in the same text box and review as a prompt. Claude writes the `SKILL.md` frontmatter (`name`, and a `description` that names the concrete moments to load the skill and when not to), the steps, and any scripts the skill needs. The review lists the files the draft changes. `e` on one of your skills opens the review with the change box open; Claude may change `SKILL.md` and the skill's other text files, and large data files are kept as they are. From the review, `e` opens `SKILL.md` in `$EDITOR`. `e` on a built-in skill saves a copy under a new name; the copy's `name` and its paths (`~/.claude/skills/<name>/…`) follow the new name. When you save an edit, jazari also updates the installed copy in `~/.claude/skills/` if it installed that copy and nobody changed it; otherwise it tells you and leaves it alone. Every edit, rename, restore and delete keeps the whole folder it replaces in `~/.jazari/skills/.versions/<name>/`, up to the newest 20 per skill; `v` lists them and `enter` restores one. Deleting a skill leaves its installed copy in `~/.claude/skills/`.

```sh
jazari build skill list                                     # built-in and your own skills, and which are installed
jazari build skill new "check that every migration has a rollback"   # write, review, save; --from <file> or stdin also work
jazari build skill edit my-skill                            # SKILL.md in $EDITOR; --with "<change>" revises the skill with Claude
jazari build skill copy easy-english --as plain-english     # a copy of a built-in skill you can change
jazari build skill rm my-skill                              # its folder is kept as a version
jazari build skill versions my-skill                        # earlier versions; add a number to print one's SKILL.md
jazari build skill restore my-skill 2                       # make v2 the skill again; without a number, the newest
jazari build skill install easy-english                     # install into ~/.claude/skills unless a copy is there
jazari build skill sync                                     # what each session start does: install missing ticked skills, update old copies
jazari build rules coordination easy-english                # a launch command with a prompt and a skill
```

Easy English's word lists are the CEFR-J Wordlist 1.5 (Yukio Tono, Tokyo University of Foreign Studies; free for research and commercial use with this citation) and the New General Service List 1.2 (Browne, Culligan and Phillips; CC BY-SA 4.0). They are included unchanged; `assets/skills/easy-english/data/SOURCES.md` has the full credits.

Claude Code reads only the last `--append-system-prompt-file` it is given, so the chosen prompts are merged into one file in `~/.jazari/launch/`, written when the command is copied and named after its role, its prompts and a hash of its text, such as `lead-coordination+output-hygiene-8873230f.md`. A session's or lead's file whose command hasn't been copied for a week is removed (a conversation keeps the prompt it started with); a teammates' file is kept, since a resumed lead still points at it. `jazari build coordination` prints the stable `~/.jazari/coordination.md`.

The prompts are added to the session's system prompt, so they survive `/clear` and compaction. They apply to new conversations only: a resumed conversation keeps the system prompt it started with. The coordination rules add about 2k tokens to each request of those sessions.

Over SSH, `c` sends the command through your terminal (OSC 52, which kitty, WezTerm, Alacritty and iTerm2 with clipboard access allow; tmux needs `set-clipboard on`) and also shows it so you can copy it by hand. `jazari build rules` lists the prompts and `jazari build rules coordination output-hygiene` prints the command (`--team` for an agent team, `--path` for only the file's path to paste into a running session, `--format json` for scripts); `jazari build coordination` is short for `jazari build rules coordination`.

`b` on a session opens its repo's board, read-only and live: agents with their last-seen times, claims, decisions, messages, conflicts and handoff notes. Agents and their claims not seen for more than 2 hours are marked `[stale]`, and `needs-human` conflicts are tagged in yellow. A relation map at the top shows how the agents are linked: messages one sent another (or everyone), conflicts between them, and claims that cover the same file. jazari never edits the board; the sessions own it. To change output hygiene, save a copy (`e` in the picker, or `jazari build prompt copy`) and tick that instead.

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

jazari keeps the best value each one ever reached, so retention pruning or an import never takes a
badge away. When a rank or level rises, the footer says so once (Esc dismisses it). `jazari watch badges`
prints the same (`--format json` or `csv` for scripts).

In the share view, `s` saves the badges as an image, `Downloads/jazari-badge-YYYY-MM-DD.png`, and
shows it in your file manager. `jazari watch badges --png [PATH]` saves it from the command line.
The image is drawn from the view itself (same art, colours and text), so it looks the same in any
terminal.

`p` also prepares a LinkedIn post: a line about today's work drafted by Claude Code, then today's,
the last 7 and 30 days', the daily average and all-time tokens, and your rank. You see the whole
post beside your badges first: `enter` shares it to LinkedIn, `c` copies the whole post (and works
again on the Done screen), `s` saves the badge image, `e` edits the line, `E` opens the whole post
in `$VISUAL`/`$EDITOR`, `r` drafts a new line, `v` shows the next of three variants of the line, and
`q`/`esc` cancels without writing or opening anything. Each variant names the features you worked
on that day (your busiest session titles, with project names taken out), and lines you shared or
copied in the last 10 days are kept in `~/.jazari/share/posts.jsonl` so new drafts don't repeat
them. Confirming:

1. saves a 1200×627 card with your badge, rank, stats, achievements and `git config user.name` to
   `Downloads/jazari-YYYY-MM-DD.png` (or `~/.jazari/cards/`); sharing again the same day
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
jazari uses a template line instead and says why. A client or company named only inside a session
title, and not in any project or folder name, is **not** removed: read the line in the preview
before you confirm. Without Claude Code, or if drafting fails or takes over a minute, the post uses
a template line built from the stats, with a note saying so.

```sh
jazari watch share --print               # print the post and write the card; opens nothing
jazari watch share --print --no-ai --out card.png
```

## How it works

- **Data** lives in `~/.jazari` (`%USERPROFILE%\.jazari` on Windows; `JAZARI_HOME` moves it). The
  settings, usage history, prompts and launch files are at the top, and Claude Code's own files
  (the event log, status line reports) are in `claude/`. Only the hooks, the `jazari-usage` skill
  and the status line live in `~/.claude`. The build part's files (its task database, the founder
  profile, product repos and work folders) are in `build/`.

- **Status** comes from [Claude Code hooks](https://code.claude.com/docs/en/hooks). Each event runs
  `jazari hook` (async, a few ms), which appends a trimmed line to `~/.jazari/claude/events.jsonl`.

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

The history is a SQLite database at `~/.jazari/usage.db`, indexed from Claude Code
transcripts (`~/.claude/projects`). It includes sessions from before jazari was installed.
Tokens are bucketed by the local day each API call happened, so long sessions split across days.

Session details (sessions, per-agent days, skill runs) are kept for the **last 30 days** by
default; `jazari watch retention` changes that. Rows stay even after Claude Code deletes their
transcripts (it does after `cleanupPeriodDays`, 30 days by default). Each session is indexed when it ends (the `SessionEnd` hook starts a background
`jazari watch sync`), so it is recorded even if the dashboard isn't open. The dashboard also re-indexes
every minute while open, and `jazari watch sync` / `report` / `sql` / `total` index on demand (only
changed files are re-read).

Retention never touches the token counter: every session's tokens per agent and day are kept
for good, so the header's today and all-time figures, `report --by day` and the Usage view's daily
chart reach back past the retention window. `jazari watch total` prints the all-time total, ready to
share, with today, the last 7 and 30 days and their daily averages:

```sh
jazari watch total                # Claude Code tokens since Sep 21, 2026: 133,533,863 (133.5M) + breakdown
jazari watch total --format json  # also a `periods` list: today, 7d, 30d, all
jazari watch retention            # show how many days of details are kept
jazari watch retention 90         # keep 90 days; older details are deleted on the next sync
jazari watch retention forever
```

The all-time total starts from the oldest day jazari indexed, so usage that was already gone from
Claude Code's transcripts when jazari first ran isn't included. Days pruned by versions before 0.4
survive only as one sum, so they count toward all time but have no daily rows.

### Export and import

```sh
jazari watch export                     # writes jazari-export-YYYY-MM-DD.json in the current directory
jazari watch export ~/jazari-backup.json
jazari watch import ~/jazari-backup.json
jazari watch import --restore ~/jazari-backup.json   # this machine's own earlier data, after a reinstall
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
jazari watch report --by skill --days 7          # skills by ≈tokens this week
jazari watch report --by session --date 2026-10-04
jazari watch report --by day --format json       # also: project, model, agent; csv
jazari watch sql "SELECT skill, SUM(total) FROM skill_runs GROUP BY skill" --format json
jazari watch schema
jazari watch status                              # live: status, activity, context left, usage limits
jazari watch status --format json --all          # include ended sessions
jazari watch system --format json                # disk, memory, CPU: room for heavy work?
```

**For Claude:** `jazari install` adds a personal `jazari-usage` skill
(`~/.claude/skills/jazari-usage/SKILL.md`). Ask any Claude Code session things like "which
skills used the most tokens this week?" or "how much context is left in my other sessions?" and
it will use `jazari watch report` / `jazari watch sql` / `jazari watch status`. Before a large build, image pull or
install, it checks `jazari watch system`. `jazari watch sql` accepts only read-only
statements.

### Known limitations

- `/btw` side questions and `!` shell commands fire no hook and leave nothing in the transcript, so
  they don't change a session's status.

- After you approve a permission prompt, the row stays on APPROVE until the tool finishes
  (Claude Code fires no hook on approval).
- Sessions started before `jazari install` show up only after they are restarted.
- The event log rotates at 20 MB (`events.1.jsonl` keeps the previous one).

## Development

```sh
cargo test
JAZARI_HOME=/tmp/jazari-dev cargo run          # use a separate event log
```

## Releasing

Releases are built by [`dist`](https://opensource.axo.dev/cargo-dist/) in
`.github/workflows/release.yml` for macOS (arm64, x64), Linux (arm64, x64) and Windows (x64).
Bump `version` in `Cargo.toml`, rename `## Unreleased` in `CHANGELOG.md` to `## X.Y.Z - date`
(dist puts that section in the release notes, and `jazari update` shows it as "What's new"),
commit, then:

```sh
git tag v1.0.0 && git push origin v1.0.0
```

After changing `dist-workspace.toml`, run `dist generate` to refresh the workflow.
