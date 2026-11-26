---
title: A waiting PermissionRequest hook keeps the terminal prompt open, and the first answer wins
date: 2026-11-26
category: integration-issues
module: answering prompts (hooks)
problem_type: integration_issue
component: hooks
symptoms:
  - A question shows in the terminal, but the user cannot answer it while a hook waits
  - The terminal shows "running PreToolUse hooks" instead of the question
root_cause: wrong_api
resolution_type: design
severity: medium
retire_when: Claude Code documents a supported way for a local program to answer a pending prompt, or changes how PermissionRequest and PreToolUse hooks wait; check code.claude.com/docs/en/hooks.md
tags: [claude-code-hooks, permission-request, pre-tool-use, ask-user-question, exit-plan-mode, prompts]
---

# A waiting PermissionRequest hook keeps the terminal prompt open, and the first answer wins

## Problem

jazari should answer a session's prompts from the dashboard or Telegram. A hook must wait for that
answer. The docs do not say whether the terminal prompt is visible while a hook waits.

## What we tested

Claude Code 2.1.332 on macOS, on 2026-11-26. Each test used a new scratch session in tmux with its
own `--settings` file. The hook waited 10 or 15 seconds, and the screen was captured while it waited.

| Hook | Prompt | Visible while the hook waits? | Result |
|---|---|---|---|
| `PermissionRequest`, no output | Bash permission | Yes | A terminal answer applied at once. The hook kept running. |
| `PermissionRequest`, `allow` after 10 seconds | Bash permission | Yes | The open prompt closed, and the command ran. |
| `PreToolUse`, answer after 15 seconds | AskUserQuestion | No. The screen showed "running PreToolUse hooks". | The answer applied, but the user could not answer in the meantime. |
| `PermissionRequest`, `allow` with `updatedInput.answers` after 10 seconds | AskUserQuestion, default mode and auto mode | Yes | The answer applied. The screen showed "Allowed by PermissionRequest hook". |
| `PermissionRequest`, `allow` after 10 seconds | ExitPlanMode | Yes | The plan prompt stayed open after the hook ended. A plain `allow` does not approve a plan. |

The hook that answered a question returned this:

```json
{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow",
 "updatedInput":{"questions":[...the original questions...],"answers":{"Pick a fruit":"Pear"}}}}}
```

For ExitPlanMode, the hook input `tool_input` has `plan` and `planFilePath`. There were no
`permission_suggestions`.

## Rules

- Answer prompts only from a `PermissionRequest` hook. Then the terminal prompt stays open, and the
  first answer wins.
- Never wait in a `PreToolUse` hook. It hides the prompt, so a person at the desk sees a session
  that does not respond.
- After a terminal answer, the hook keeps running until its own wait ends. The hook must notice the
  terminal answer and then stop.
- Plan approval cannot be answered with a plain `allow`. Leave plan prompts to the terminal until a
  test finds an output that works.

## Prevention

After a Claude Code update, repeat the AskUserQuestion test in a scratch session. The test passes
if the question is visible while the hook waits and the hook's answer applies.

### How to run a live test safely

Start each scratch Claude session with its own config folder, for example
`CLAUDE_CONFIG_DIR=<scratch dir>`, and a scratch `HOME` if needed. Then nothing reads or writes the
real `~/.claude`. Never run a debug jazari from a session that loads the real settings.

On 2026-11-26 a scratch session loaded the real settings and ran a debug jazari on `SessionStart`.
That hook re-registered jazari's hooks in the real `~/.claude/settings.json` with the debug
binary's path, so every session on the machine ran unmerged code until `jazari update` fixed it.
`JAZARI_HOME` does not protect `~/.claude`.
