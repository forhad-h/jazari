# Changelog

`jazari update` shows the bullets under a version as "What's new", and `dist` copies them into the
GitHub release. Keep each bullet to one short line.

## Unreleased

- Telegram pairing codes are 10 letters and digits, and each chat gets one try
- `jazari build service install|uninstall|status` runs the Telegram bridge and the supervisor in the background on macOS, from login

## 1.0.1 - 2026-11-26

- Telegram message text no longer shows in the process list: curl now reads it on stdin
- Autonomous jobs no longer queue a reworded copy of work that was done in the last 30 days
- The dashboard restores the terminal when it is stopped by a signal or its tab is closed, so mouse moves are no longer typed into the shell

## 1.0.0 - 2026-11-26

- First release: jazari runs a small AI software company on your Claude subscription, and watches every Claude Code session on your machine
- `jazari build new <name> <idea>` plans a product (vision, roadmap, ADR, threat model), then builds it task by task with 37 AI roles
- Every branch passes harness-run checks, a secret scan, an independent evaluator, code review and security review before the merge queue merges it
- Workers run in a locked-down Docker container per product, in their own clone, and never see your repo or its `.git`
- Two modes per product: normal stops after your tasks; autonomous keeps hunting bugs, fixing vulnerable dependencies and improving UX
- Telegram: pair a chat, get short cards with buttons, answer questions, approve deploys, spending and contact with users
- jazari learns how you like the work done, adapts when you change your mind, and never learns a rule that weakens quality
- jazari sleeps until your 5-hour or 7-day usage window resets, keeps a share for you, and spreads the week evenly
- `jazari watch`: the live dashboard, usage history and reports, badges and a share card, and guards for disk, memory and risky commands
