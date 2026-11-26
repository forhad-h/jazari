---
title: Claude Code's cross-session socket delivers a message to a running session at once
date: 2026-11-26
category: integration-issues
module: session messages (channel)
problem_type: integration_issue
component: messaging
symptoms:
  - A message from `jazari watch tell` waits until the session makes its next tool call or the user types a prompt
  - An idle session never gets a message from jazari
root_cause: wrong_api
resolution_type: code_fix
severity: medium
retire_when: Claude Code documents the full line format of the cross-session socket, or adds a documented way to inject a user message into a running interactive session; check code.claude.com/docs/en/cross-session-messaging.md
tags: [claude-code, cross-session-messaging, unix-socket, tell, inbox]
---

# Claude Code's cross-session socket delivers a message to a running session at once

## Problem

`jazari watch tell` puts a message in the session's inbox file. The PreToolUse or
UserPromptSubmit hook then gives the message to Claude. So the message waits for the next tool
call or the next prompt. A session that is idle never gets it.

## What we checked

These facts were tested with Claude Code 2.1.332 on macOS, on 2026-11-26.

| Way | Reaches a session that the user already started? | Reaches an idle session? |
|---|---|---|
| Cross-session socket | Yes | Yes. The session starts a new turn. |
| Channels (an MCP server with `claude/channel`) | No. The session must start with `--channels` or `--dangerously-load-development-channels`. | Yes |
| Remote Control | Only with `--remote-control`, and only from claude.ai or the Claude app | No local client |
| `--input-format stream-json`, Agent SDK | No. Only for `claude -p` runs that the caller starts | Yes |

Channels are a research preview. They need claude.ai or Console login.

## How the socket works

1. Every interactive session listens on a Unix socket, for example `/tmp/cc-socks/<pid>.sock`.
   The socket has mode 600. A sessions directory of `/private/tmp/cc-socks`, `/run/user/<uid>/cc-socks`
   or `cc-socks-<n>` is also possible.
2. Claude Code writes `~/.claude/sessions/<pid>.json`. The fields that jazari needs are `pid`,
   `sessionId`, `messagingSocketPath`, `status` (`busy`, `idle`, `shell`) and `peerFeatures`.
3. Claude Code writes `~/.claude/sessions/<pid>.<hash>.key` (mode 600). This file is JSON with
   `peerToken`, `procStart` and `pidDomain`.
4. The protocol is one JSON object per line. Claude Code's own debug help text gives this example:

   ```
   {"type":"auth","token":"<token>"}
   {"type":"user","message":{"role":"user","content":"<text>"}}
   ```

5. The auth line is optional on macOS and Linux. It is required on Windows.
6. Two tokens exist. `CLAUDE_CODE_MESSAGING_TOKEN`, which hooks and Bash commands get, is the
   child token. It differs from `peerToken` in the key file. A process that is not a child of the
   session, like jazari, sends `peerToken`.
7. A connection that sends no complete line within a short time is closed.

## What the receiver does

- The message arrives at once, also while the session is busy. Claude sees it as a message from
  another Claude session. So the text must say that it comes from the user through jazari.
- An idle session starts a new turn with the message.
- The socket writes no reply. A connection that closes without an error is the only signal that
  the sender gets. If the receiver holds or refuses the message, only the receiving session is told.
- The setting `crossSessionInbound` (`accept`, `hold`, `refuse`) decides what happens to messages
  from outside. The default depends on the permission mode. A message never approves a tool call.
- In the transcript the message is a user entry with `origin.kind: "peer"`.

## How jazari uses it

The socket is only a wake-up signal. The message itself always goes through the inbox. There are
three reasons:

- The receiver treats a socket message as a message from another session. Such a message does not
  have the user's authority. The hook delivers an inbox message as a message from the user.
- The receiver can hold a socket message for approval.
- The socket sends no receipt, so jazari cannot know that Claude got the text.

So `jazari watch tell` works in this order:

1. Write the message to the inbox, as before.
2. If the session's `status` is `idle`, send one short nudge with no message text. For example:
   "jazari has a message for you from the user. Run any tool call (for example
   `jazari watch status`) to receive it."
3. The session runs a tool call. Then the hook delivers the real text once, with the user's
   authority.
4. Report what happened in plain words: "queued; nudged the idle session" or "queued for its next
   tool call".

Rules for the nudge:

- Use a socket only if it is listed in `~/.claude/sessions/*.json`. Its pid must be alive, and the
  current user must own the socket file. Never take a socket path from message text.
- A timeout or an error means that no nudge was sent. The message still waits in the inbox.
- Stop requests stay on the hook path, because a stop must refuse the next tool call.
  `--interrupt` stays SIGINT.
- On Windows, do not nudge. Use the inbox only.

## The peer token is Claude Code's internal credential

`peerToken` in `~/.claude/sessions/<pid>.<hash>.key` is a secret that Claude Code uses between its
own sessions. It is not documented for other programs. On macOS and Linux the auth line is optional,
so a nudge works without the token. jazari reads `peerToken` only if the user turns on a setting.
This setting is off by default.

## Prevention

The line format is not documented. After a Claude Code update, send one test line to a scratch
session and check that it arrives. If it does not arrive, idle sessions are not woken. But `tell`
still works, because the message waits in the inbox.

## Related

- https://code.claude.com/docs/en/cross-session-messaging.md
- https://code.claude.com/docs/en/channels.md
- `claude-code-hooks-miss-interrupts-and-background-shells.md`
