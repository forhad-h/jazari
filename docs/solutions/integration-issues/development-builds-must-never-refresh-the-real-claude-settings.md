---
title: Development builds must never refresh the real Claude Code settings
date: 2026-11-26
category: integration-issues
module: hook refresh (install.rs, hook.rs, update.rs)
problem_type: integration_issue
component: settings
symptoms:
  - "All jazari hooks in ~/.claude/settings.json ran a debug build from a worktree's target/debug/"
  - "A hook kind from an unmerged branch (PermissionRequest → watch answer --hook) stayed in the real settings"
  - "Nothing was wrong until the founder's sessions ran unmerged code"
root_cause: wrong_scope
resolution_type: code_fix
severity: high
tags: [hooks, settings-json, refresh, dev-build, jazari-home, e2e]
---

# Development builds must never refresh the real Claude Code settings

## Problem

Since 1.0.3, the SessionStart hook runs `install::refresh`: it adds the hooks a newer version
registers to `~/.claude/settings.json`, with the path of the binary that runs the hook.

An end-to-end test started a scratch Claude Code session with `--settings <scratch file>` and
`JAZARI_HOME=<scratch folder>`. That session's SessionStart hook ran a debug build
(`…/target/debug/jazari hook`). `--settings` adds settings for that session only; the refresh
still writes `crate::claude_dir()/settings.json`, which is the real file in the user's home folder.
`JAZARI_HOME` moves jazari's data, not Claude Code's settings. So the refresh rewrote all 19 hooks
in the real settings with the debug path, and added the debug build's new hook kind.

## Why it was possible

1. Any binary that ran the hook could refresh. Nothing checked that it was the installed jazari.
2. A hook counted as jazari's only when its command contained "jazari" and ended in a known
   argument. A newer kind (`watch answer --hook`) was not recognized, so a later refresh by the
   installed binary couldn't remove it.

## Fix

- `install::skip_reason`: a refresh does nothing, and says why, when
  - `JAZARI_HOME` is set (a scratch or test run);
  - the binary is under a cargo `target/` folder (one with `CACHEDIR.TAG`, or at
    `target/<profile>/jazari`);
  - the binary is not the one recorded in `<data dir>/installed-hooks.json` (`exe`, written by
    `install` and by every refresh). `jazari update` passes the binary it just installed with
    `install --refresh --exe <path>`, so an install in a new place still works.
- Ownership by file name: every hook or status line entry whose command runs a file named `jazari`
  (or `jazari.exe`) is jazari's, at any path. A refresh by the installed binary replaces all of
  them with its own set. So stray paths and unknown kinds (a newer version's, after a rollback) are
  removed. Hooks that the user removed stay removed, through the record.

## Rule

Development and tests never write to `~/.claude`. Code that writes there must check that it runs
as the installed binary, and must not trust `JAZARI_HOME` or `--settings` to redirect the write.
A new file that jazari writes under `~/.claude` needs the same check.
