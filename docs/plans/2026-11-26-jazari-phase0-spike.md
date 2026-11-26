# jazari Phase 0 spike: findings

Date: 2026-11-26. Claude Code 2.1.332, Docker 29.8.0, macOS host.

## 1. Headless Claude in Docker on a subscription token: works

- Image: `assets/jazari/Dockerfile` (node:22-slim, git, curl, jq, `@anthropic-ai/claude-code` pinned). The image is 1.06 GB and runs as the non-root `node` user.
- Auth: `CLAUDE_CODE_OAUTH_TOKEN` (from `claude setup-token`) passed with `-e`. No other login is needed.
- `claude -p … --permission-mode bypassPermissions` runs tools without prompts inside the container (Write, Bash). `permission_denials` is empty. A trivial Haiku run needs well under `--memory 1g`.

## 2. Usage-limit signal: use `stream-json`, not the status line

- `--output-format json` has no rate-limit fields. Headless runs do not feed the status line either.
- `--output-format stream-json --verbose` emits a line like this:

  ```json
  {"type":"rate_limit_event","rate_limit_info":{
    "status":"allowed","resetsAt":1795680600,"rateLimitType":"five_hour",
    "overageStatus":"rejected","isUsingOverage":false,
    "unifiedWindows":{
      "five_hour":{"utilization":0.08,"resetsAt":1795680600,"status":"allowed"},
      "seven_day":{"utilization":0.45,"resetsAt":1795752000,"status":"allowed"}}}}
  ```

- **Decision:** workers always run with `stream-json`. `limits.rs` reads `unifiedWindows` from every run, so the 5-hour and 7-day usage, the reset times and the status come from the workers themselves.
- When a window's `status` is not `allowed`, the supervisor sleeps until that window's `resetsAt`, plus jitter.
- The status-line snapshots remain a second source, from the founder's interactive sessions.
- `overageStatus: rejected` means no paid overage can happen on this account. This matches the subscription-only rule.
- Not seen yet: the exact event and result when a window is exhausted. Treat any `status` other than `allowed`, or a run that ends with `is_error` and no `rate_limit_event`, as "sleep and probe later" (15 min, 30 min, then 1 h).

## 3. Telegram

- The bot token is read from the Keychain service `marsad-jazari-telegram-token`.
- `getMe` returns `ok`, and `can_join_groups=false`.
- **HTTP client decision: curl, no new crate.** The URL, which holds the token, is fed to `curl -K -` on stdin, so the token never appears in the process arguments. This matches marsad's existing ntfy/webhook curl pattern.
- **Long-poll:** run `getUpdates?timeout=50&offset=<last+1>` in a loop on a thread.
- **Round trip works:**
  1. The founder sent `/start` from a private chat.
  2. `getUpdates` received it.
  3. The chat id was stored in the Keychain service `marsad-jazari-telegram-chat`.
  4. `sendMessage` was delivered.
  5. The update was acknowledged with `offset=<update_id+1>`.
- **Phase 1 pairing:** replace "first `/start` wins" with the one-time-code pairing from the plan. Accept only `chat.type == "private"` from the stored chat id.

## 4. Consequences for Phase 2

- `worker.rs` parses `stream-json` line by line. It takes:
  - `rate_limit_event`, which goes to `limits.rs`;
  - the final `result`, for `subtype`, `is_error`, `usage` and `structured_output`.
- The container gets the repo at `/work` and nothing else. The Claude token goes in through the environment, read from the Keychain service `marsad-jazari-claude-token` when the container starts.
