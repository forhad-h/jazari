# Answer Claude Code prompts from jazari

## Goal

The founder stays in the jazari dashboard. From there they see every session's permission prompt
and question, answer them, and send messages, for many sessions at once. The terminal prompt stays
open too, and the first answer wins.

Telegram controls only jazari's own sandbox work. Sessions that the founder started by hand are
controlled only from the dashboard on the machine.

In jazari's Docker sandbox, jazari answers routine questions by itself. Every other question goes
to the founder.

This plan has two parts:

- Part A: the founder answers prompts of host sessions, from the dashboard only.
- Part B: jazari answers routine questions of its own sandbox workers.

The socket nudge for `jazari watch tell` is a separate change. Its design is in
`docs/solutions/integration-issues/claude-code-cross-session-socket.md`.

## What the tests showed

The full results are in
`docs/solutions/integration-issues/claude-code-permission-request-hook-races-the-terminal-prompt.md`.

- A `PermissionRequest` hook can wait. The terminal prompt stays open, and the first answer wins.
- A `PermissionRequest` hook can answer AskUserQuestion with `updatedInput.answers`.
- A waiting `PreToolUse` hook hides the prompt. Never use it to wait.
- A plain `allow` does not approve ExitPlanMode. Plan prompts stay terminal-only in this plan.

## Part A: the founder answers host prompts

### Rules

1. Only the founder answers, with a key press in the dashboard on the machine. jazari never answers
   a host prompt by itself, by rule or by a learned preference. Telegram shows no card, button or
   answer for a host prompt.
2. If the wait ends with no answer, the hook writes nothing. Then the terminal prompt stays, and
   the session waits for the user as before. No answer never means "allow".
3. The feature is off by default. The founder turns it on for all sessions or for one session.
4. The wait is short, 120 seconds by default, and set in config. Each waiting hook is a running
   process, so the wait has a limit.
5. jazari never turns a deny into an allow. A deny and "leave it to the terminal" are always safe.

### The hook

1. `install.rs` adds a second, synchronous hook on `PermissionRequest` only:
   `jazari watch answer --hook`. Its `timeout` is the wait plus 10 seconds. The async event hook
   stays as it is. `jazari install` adds the hook only when the setting is on.
2. The hook reads the input: `session_id`, `tool_name`, `tool_input`, `permission_suggestions`.
   If the feature is off for this session, or the tool is ExitPlanMode, the hook exits at once with
   no output.
3. The hook writes a request file in jazari's data dir: `prompts/<session>/<id>.json`, mode 600.
   The file holds a random one-time code, the tool, a preview, the options and the start time.
   The preview is redacted with `telegram::redact`, so that no secret is stored.
4. The hook checks for an answer file every 500 ms until the wait ends.
5. The hook stops early when the terminal answered first. The signal is a later hook event from the
   main agent of the same session, after the request's start time. Examples are PostToolUse,
   PreToolUse, Stop and UserPromptSubmit. The spike showed that the hook keeps running after a
   terminal answer, so this check is needed.
6. When the hook stops, it closes the request with one outcome: `dashboard`, `desk` or
   `timeout`. A late answer to a closed request does nothing.

### What the hook returns

| Prompt | Founder's answer | Hook output |
|---|---|---|
| Tool permission | Allow | `decision.behavior: "allow"` |
| Tool permission | Always allow | `allow` plus `updatedPermissions`. Only a rule from the request's own `permission_suggestions` can be used. |
| Tool permission | Deny | `decision.behavior: "deny"` and `message: "Denied from jazari by the user."` |
| AskUserQuestion | An option per question | `allow` and `updatedInput` with the original `questions` and `answers`. `answers` maps each question text to an option label. For multi-select, the labels are joined with ", ". |
| ExitPlanMode | none | No output. The terminal answers. |

Free-text answers ("Type something") are not in the first version. A test must first show that
`answers` accepts text that is not an option label.

### The dashboard

1. A session in APPROVE or QUESTION with an open request shows a mark in the list.
2. The details view shows the request: the tool and the full command or file path, or each
   question with its options.
3. Key `a` opens the answer view. For a permission prompt the keys are `y` (allow), `A` (always
   allow, only when a suggestion exists) and `n` (deny). For a question the founder picks an option
   for each question, then presses Enter.
4. The answer view writes the answer file. It does not call the hook directly.
5. The view keeps `t` (tell) next to `a`, so that the founder can answer and send messages to many
   sessions from one place.
6. Update the footer key hints in `ui.rs`, the README key list, and `CHANGELOG.md`.

### Telegram

Telegram is not part of Part A. Telegram for manual sessions is a later follow-up, only if the
founder opts in.

### The record

Every outcome is written to the audit record (`src/jazari/audit.rs`) and to the actions log
(`act::record`). The entry has the session, the tool, the preview, the answer and who answered.

## Part B: jazari answers routine sandbox questions

Sandbox workers run `claude -p` with `bypassPermissions` in Docker (`src/jazari/worker.rs`). So
they have no permission prompts. When a worker needs a decision, it returns `needs_founder`.
`src/jazari/supervisor.rs` (the `Kind::Blocked` branch) then shows the question on the dashboard
and pushes a Telegram card, through the existing Ask flow. Part B adds one step before that.

### Rules

1. Deny by default. jazari answers only when a deterministic check says that the question is
   routine. A model alone never decides this.
2. These go to the founder:
   - any `approval_needed` (deploy, spend, contact)
   - any `hard_line` question
   - a question with no options
   - any match of the risk word list in the question, the `why` text, or any option's label or
     effect
3. The worker writes the question, so a worker can make a risky question look routine. For this
   reason the check reads every option too. One matching option sends the question to the founder.
4. When the check is not sure, the founder gets the question.
5. An auto-answer picks the worker's own recommended option. If there is no recommended option,
   the question goes to the founder. jazari never picks the first option instead, because that
   option is only the worker's choice. An auto-answer never writes free text.
6. An auto-answer is not pushed to Telegram.
7. Each auto-answer is written to the audit record and the actions log. The dashboard shows it as
   "answered by jazari". So the founder can review it and give another answer with `/answer`.
8. A setting per product turns auto-answers off. The default is on, because every jazari product
   runs in the sandbox.

### The risk word list

The check matches whole words and ignores case. The first list:

- secret, token, credential, key, api key, password, private key, certificate
- user data, personal data, PII, customer data, email address, phone number
- auth, login, session, permission, role, access, admin
- security, vulnerability, CVE, encryption
- payment, billing, price, invoice, spend, cost, money, subscription
- delete, drop, truncate, wipe, purge, remove data, migration
- network, egress, external, webhook, send email, send SMS, notify users, contact
- deploy, production, release, publish, DNS, domain

The list lives in one constant in a new module, `src/jazari/routine.rs`, with its tests.

### Tests

- A routine question with routine options is answered with the recommended option.
- A question with routine words but one option that names a hard-line action goes to the founder.
  One example is "Use library A or B?" with the option "B: also rotates the API key". Another is
  "Which file name?" with the option "drop the users table first".
- A question that is marked `hard_line` or carries `approval_needed` goes to the founder.
- A routine question with no recommended option goes to the founder.
- A question with no options goes to the founder.
- The word check ignores case and matches whole words only.
- A product with auto-answers turned off sends every question to the founder.

## Files

| File | Change |
|---|---|
| `src/install.rs` | The synchronous `PermissionRequest` hook, only when the setting is on |
| `src/answer.rs` (new) | The hook, request and answer files, the outcome, the output JSON |
| `src/settings.rs` | `answer_prompts` (off), `answer_wait_secs` (120) |
| `src/app.rs`, `src/ui.rs` | The answer view and key `a` |
| `src/jazari/routine.rs` (new) | The deterministic check for Part B |
| `src/jazari/supervisor.rs` | Part B: check before the founder card |
| `src/jazari/store.rs` | The per-product auto-answer setting |
| `README.md`, `CHANGELOG.md`, `assets/SKILL.md` | Keys, commands and the new setting |

Tests never start Claude Code, never open Telegram and never touch the real data dir. The hook's
clock and file reads are injected, like `share::Desktop::for_tests()`.

## Order of work

1. This plan and the test note, as a doc PR.
2. Part A: hook and request files, then the dashboard. Each step is its own PR.
3. Part B, as one PR.

## Verification

1. `cargo test` and `cargo clippy --all-targets`.
2. A scratch session in tmux with `--settings` and `MARSAD_HOME=<scratch dir>`:
   - A Bash prompt answered from the dashboard runs the command.
   - The same prompt answered in the terminal first closes the request as `desk`.
   - A question answered from the dashboard gives Claude the chosen option.
   - A prompt with no answer shows the terminal prompt after the wait, and nothing is allowed.
3. A crafted sandbox question that hides a hard-line option produces a founder card.

## Open points

- ExitPlanMode: find a hook output that approves a plan, then add it.
- Free-text answers to questions.
- Windows: the hook works the same way, but this plan was tested on macOS only.
- Telegram for manual sessions: a later follow-up, only if the founder opts in.
