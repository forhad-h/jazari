# Action gate: follow-ups

The action gate (`src/act.rs`) checks and records what marsad does: messages, stops, interrupts,
ending sessions after a handoff, cleanups, settings, updates and installs. It asks the user
before a session runs a marsad command that acts. These are the next steps, roughly in order.

## Next

1. **Actions view in the dashboard.** A key (`A` is taken in the cleanup view; pick a free one)
   opening the last entries of `actions.jsonl`, with the pause state in its title and a key to
   pause or resume. Footer hints, `README.md` key list.
2. **Check handoff prompts before they start a session.** The prompt a session writes becomes the
   new session's first user message. Cap its size, drop control characters, and flag lines that
   ask to turn guards off, delete data, or send secrets out (reuse `risk.rs` patterns). Show the
   flagged lines and ask before opening the terminal, or open it with a warning note.
3. **Record the hook's asks.** The hook can't see the user's answer, but it can record that it
   asked (`outcome: asked`), so `marsad actions` shows the full trail of what a session tried.
4. **Record ballast deletion and the pressure refusals.** Both happen without a trace today. The
   watcher's `Env` needs an injectable recorder so its tests keep writing nowhere.

## Later

5. **Trust in `events.jsonl`.** Pids and start times come from a file any session can append to.
   Options: record the pid only from `CLAUDE_PID` in hooks and ignore later pid changes for a
   session; or keep a separate pid file per session written by the SessionStart hook with
   `create_new`, and trust only that.
6. **Stronger session detection.** `CLAUDECODE`, `CLAUDE_CODE_SESSION_ID` and a Claude Code
   ancestor mark a run as from a session. A command run with `!` in Claude Code counts as a
   session too; consider treating `!` runs (no tool call in flight) as the user's.
7. **Wider shell coverage in the hook.** The check reads the command text: a script file, an
   alias or a variable that expands to `marsad … --stop` gets past it. Pair it with the command
   check's `executed_files` to read scripts a command runs.
8. **Docker prune in-use check.** `builder prune` and `image prune -a` don't look at what a
   running build needs. Skip them while `docker build` or `docker compose` runs.
9. **Settings backup.** Keep the last few versions of `settings.json` (and `~/.claude/settings.json`
   on install), with `marsad settings restore`.
10. **Update integrity.** Check the installer's checksum against the release before running it.
11. **Per-sender message rate limit.** Today a session can fill another's inbox to 20 waiting
    messages; add a limit per sending session (for example 10 in 10 minutes).
