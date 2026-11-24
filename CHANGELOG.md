# Changelog

`marsad update` shows the bullets under a version as "What's new", and `dist` copies them into the
GitHub release. Keep each bullet to one short line.

## Unreleased

- The System panel shows load only when it passes twice the core count, with the multiple of cores

## 0.14.5 - 2026-11-24

- A row above the footer shows the selected session's context and the 5h and 7d limits, like Claude Code's status line

## 0.14.4 - 2026-11-24

- On macOS, the System panel and `marsad system` show free disk space as Finder does
- The System panel and `marsad system` list CPU, memory, swap, then disk
- `marsad system` measures CPU over one second, so a single spike no longer skews it
- Key hints inside windows show the key bold and what it does muted, like the footer

## 0.14.3 - 2026-11-24

- The System panel shows swap on its own line, with how much is used of the total

## 0.14.2 - 2026-11-24

- The header shows when the 5h and 7d limits reset again, and no longer repeats context left

## 0.14.1 - 2026-11-24

- Hints, units and other secondary text use a lighter gray that is easier to read
- Panel borders are darker than text, so the frame no longer competes with what it holds
- On a narrow terminal the header keeps the context left and drops reset times and totals first
- The footer keeps the most-used keys and quit on a narrow terminal, with … where keys were left out
- Token totals of a billion or more read like `15.9B`, matching the header
- Memory on macOS is counted the way Activity Monitor does, so the meter and the memory guard no longer read several GiB too free

## 0.14.0 - 2026-11-24

- Every action marsad takes (messages, stops, interrupts, cleanups, settings, updates) goes through one gate
- `marsad actions` lists what marsad did, refused or held, and who asked
- `marsad pause [--for 2h]` / `marsad resume` hold everything marsad does on its own
- A session gets a permission prompt before a marsad command that stops sessions, removes items or changes a guard
- A message from another session is marked as not from you
- The same interrupt can't fire twice within 30 seconds
- `marsad disk --guard off` also stops the watcher's emergency interrupts
- `MARSAD_DRY_RUN=1` shows what any action would do without doing it
- The panel below the session list splits in two: what the session is doing now, its id and model, and a System panel
- Disk, memory and CPU meters move from the header into the System panel
- The selected session's context shows in the header beside the 5h and 7d limits
- `enter` opens the session's details (was history): tokens, mode, folder, agents and skills above its history

## 0.13.0 - 2026-11-23

- Run `marsad install` again after updating: it adds the new guard, message and handoff hooks
- At 80% context a session is asked to write a handoff and pass it to `marsad handoff`
- `marsad handoff` opens a new session in this terminal, else the default terminal, else tmux
- Warp, iTerm2, Terminal.app, Ghostty, WezTerm, kitty, Windows Terminal and tmux are supported
- `marsad handoff --close` ends the old session once the new one is open (`--auto-close on` makes it the default)
- `marsad handoff list` and `resume <id>` show and restart saved handoffs
- The context warning also reaches long turns between tool calls
- `marsad install` offers to install tmux when it is missing
- Sessions stop before their next tool call when the disk is nearly full (95% or 8 GB free), so it can't hang the machine
- At 90% or 15 GB free, sessions are told once to finish the step and write a handoff
- `marsad disk` shows free space and the levels; `--guard off` only warns
- `marsad system` shows disk, memory, swap and CPU, and what to be careful about before heavy work
- The dashboard has disk, memory and CPU meters under the header
- All-time tokens read in B once past a billion
- A session stopped for disk space shows why, with its resume command
- A background watcher checks the disk every 20 s while sessions run and notifies once per level
- At the emergency level (98% or 3 GB free) running sessions are interrupted
- Optional ballast file (`disk.ballast_gb`) that is deleted at the emergency level to free space
- Optional phone notifications through ntfy or a webhook (`disk.ntfy`, `disk.webhook`)
- `marsad disk` shows the watcher, the ballast and phone notifications
- `D` and `marsad clean` list caches, build output and Docker data that can go, by tier, with sizes
- Rebuildable caches and build output are cleaned on their own once the disk reaches the warning level
- Backups, unused Docker volumes and merged worktrees are removed one at a time, after a confirmation
- `J` asks Claude whether each data-holding item can go; it only advises
- marsad's own CPU, memory and data folder size show beside the meters
- Tool calls that delete the home folder or root, wipe a disk, or send secrets out are refused
- `curl | sh`, decode-and-run, persistence, security changes and remote shells ask first
- File writes are checked too, including package install scripts that download or `eval` code
- Package names one letter away from a popular package ask "did you mean"
- `marsad risk` lists findings; `--check` tests a command; `--guard off` turns the check off
- Optional, off by default: OSV lookups for malicious packages, and ClamAV scans of fresh files before they run
- A session goes idle once background agents that outlived its turn finish, instead of staying active
- Session history lists the newest events first
- Sessions keep their place in the list: working ones sit on top, the rest stay in first-seen order
- Share: three post variants (`v` cycles), each naming the features worked on and avoiding your posts of the last 10 days
- The session list shows each session's name, with its project where the activity was
- The details pane is shorter and shows the full session id; `y` copies it
- The agent board shows a relation map: who messages whom, conflicts, and overlapping claims
- Usage limit reset times use a 12-hour clock (`7:20pm`, `Fri 10:00am`)
- `P` opens a small window that stays on top of other apps with every session's status (macOS)
- The `c` picker's last step can copy only the prompts' file path, to paste into a running session (`marsad rules --path`)
- Heavy commands wait while memory or CPU is under pressure; `marsad system --wait` returns once there is room
- `t` sends a session a message it reads at its next step; `x` stops it before its next tool call; `X` interrupts it now
- `marsad say` lets a session post what it is doing to the dashboard (`--attention` when it needs you)
- New built-in prompt, Working with marsad, for sessions that report to and take direction from marsad
- Disk warnings name the largest things to free; the dashboard says once a day what can be cleaned
- The cleanup also finds old Claude Code versions, stale session scratch folders and idle virtualenvs
- A report-only group lists space owned by apps and tools, with the command that frees it
- The details pane shows the command check's latest finding for the session

## 0.12.0 - 2026-11-23

- Every edit, rename and delete of your prompts keeps a version; `v` in the `c` picker lists and restores them
- `u` in the `c` picker brings back the prompt just deleted
- `marsad prompt versions` and `marsad prompt restore` list and restore versions from the command line
- The `c` picker tags each prompt `[built-in]` or `[yours]`, and so does `marsad rules`
- `esc` on an unsaved draft asks before discarding it

## 0.11.0 - 2026-11-23

- marsad keeps its data in `~/.marsad`; the data in `~/.claude/marsad` is copied over once and left there
- Claude Code's own files (event log, status line reports) are in `~/.marsad/claude/`
- Launch files have names you can read, such as `lead-coordination+output-hygiene-8873230f.md`
- The old `~/.claude/marsad/coordination*.md` paths keep working; a later release stops updating them

## 0.10.0 - 2026-11-23

- `n` writes a new prompt from a rough idea with Claude, from the session list or the `c` picker
- The idea box has Write prompt and Cancel buttons; `tab` reaches them
- The review shows what Claude assumed and the questions it left open; a number answers one
- `e` in the picker edits your prompt, or saves a copy of a built-in one; `d` deletes yours
- `marsad prompt new|edit|copy|rm` does the same from the command line
- The `generator_model` setting picks the model that writes prompts (default `fable`)

## 0.9.0 - 2026-11-23

- `c` lists your own prompts too: add `.md` files to `~/.claude/marsad/prompts/`
- After ticking prompts, choose a normal session or an agent team
- Agent team: the lead passes the prompts on to every teammate it spawns; coordination stays with the lead
- `marsad rules <names…> --team` prints the team-lead command

## 0.8.1 - 2026-10-28

- The badge art is square in any terminal font, sized from the terminal's cell shape

## 0.8.0 - 2026-10-28

- `p` opens one share view: your badges beside the post, ready to share to LinkedIn
- In the share view, `c` copies the whole post and `s` saves the badge image
- The `B` badge view and its `L` key are gone; their features now live in the share view
- The badge art keeps a fixed square size instead of stretching with the terminal

## 0.7.0 - 2026-10-27

- In the badge view, `s` saves it as an image in Downloads and shows it in your file manager
- `L` saves the image and opens LinkedIn's post box, ready for you to attach it
- `marsad badges --png` saves the image from the command line
- Ranks spread over about four years: Apprentice 1B, Practitioner 10B, Specialist 40B, Expert 100B, Master 200B, Grandmaster 400B
- Achievements to match: streak 14/60/180 days, skills 25/75/150, subagents 1k/10k/50k, busiest day 2B/5B/10B

## 0.6.0 - 2026-10-27

- Ranks are rebased for a multi-year climb: Apprentice at 100M up to Grandmaster at 400B tokens
- Achievement levels rise to match: streak 7/30/365 days, skills 10/40/100, subagents 500/5k/25k, busiest day 1B/5B/20B
- Your rank and some levels may go down once; rank-ups from the new level are announced as before

## 0.5.1 - 2026-10-27

- Table columns stay aligned when a row shows Bengali or other complex-script text
- Complex-script text in the last columns no longer draws past the screen edge
- The LinkedIn share line describes the kinds of work done instead of only counting sessions
- The share line stays generic: no session titles are sent, and lines with security or environment details fall back to the template
- A drafted share line is no longer rejected for saying "Claude Code"

## 0.5.0 - 2026-10-26

- `c` opens a picker: start Claude Code with coordination, output-hygiene rules, or both
- `marsad rules` lists the rules files and prints the command for any set of them
- Success notes in the footer clear themselves after a few seconds

## 0.4.0 - 2026-10-24

- Questions show QUESTION instead of PERMISSION, and PERMISSION is now APPROVE
- Pressing Esc on a prompt, or interrupting a turn, sets the session back to IDLE
- Sessions stay ACTIVE while a background shell Claude started is still running
- Sessions whose process exited show ENDED reliably
- Sounds and desktop notifications when a session needs you or finishes; `m` toggles them
- The header shows today's and all-time tokens
- A permanent per-day token counter: retention now prunes session details only
- `marsad total` adds today, 7 and 30 days with daily averages
- `marsad context` shows how full a session's context is
- Claude is told once when a session's context passes 80%, so it can write a handoff (`marsad context --warn`)
- `marsad export` and `marsad import` back up or move all data; importing twice never double counts
- `marsad import --restore` brings back this machine's own data after a reinstall
- Coordination rules let several sessions in one repo share an `AGENT-BOARD.md` and avoid editing the same files
- `c` copies the command that starts a session with them (`marsad coordination` prints it)
- `b` shows a repo's agent board: who is working on what, stale claims and `needs-human` conflicts
- `B` shows your rank, Novice to Grandmaster by all-time tokens, and bronze, silver and gold achievements
- Badges never go down: pruning and imports can't take one away; `marsad badges` prints them
- The footer announces a new rank or achievement level once
- `p` prepares a LinkedIn post and a 1200×627 card, with a line Claude drafts from today's redacted work
- The share preview lets you edit the line or the whole post, redraft it, or cancel before anything is saved
- `marsad share --print` prints the post and writes the card without opening anything
- Skills pruned before this version whose transcripts are gone don't count toward Distinct skills

## 0.3.2 - 2026-10-11

- Session status is right-aligned against the table's right edge

## 0.3.1 - 2026-10-11

- Session status shows its label first and the icon after it (`ACTIVE ◐`)

## 0.3.0 - 2026-10-11

- `marsad update` installs the latest release and shows what changed
- Automatic updates: `marsad update --auto on`
- The dashboard shows a yellow notice when a new version is out
- `s` / `S` sort the session list by status, project, context, time or tokens
- Subagents for each session (`tab`): launching skill, tokens used, context left
- Skill windows stay open when a background subagent finishes, so skill token counts are more accurate
- Session status sits at the right edge, so it stays visible when another window covers the left
- TIME shows how long each session has spent working, idle time excluded, across resumes
- Usage history outlives Claude Code's transcripts; sessions are recorded as soon as they end
- `marsad retention` sets how many days of history to keep (default 30)
- `marsad total`: all-time token total, kept after old days are pruned
- Usage view: totals, daily chart and all-time total; `p` switches the period
- 5h / 7d usage limits show the local time they reset (header, status line, `marsad status`)
- Session history (`enter`): prompts, replies, skills, subagents with their reports, questions with answers
- A subagent handing back its report no longer closes the current skill window

## 0.2.0 - 2026-10-09

- Context left before auto-compact, and 5h / 7d usage limits
- Skill history for each session (`enter`)
- 30-day usage history (`u`), `marsad report`, `marsad sql`
- `marsad status` for live sessions

## 0.1.0 - 2026-10-09

- First release: live dashboard of every Claude Code session
