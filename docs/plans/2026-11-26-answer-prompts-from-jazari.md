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
3. The feature is off by default. The founder turns it on with `jazari watch answer --on`.
4. The wait is short, 120 seconds by default, and set in config. Each waiting hook is a running
   process, so the wait has a limit.
5. jazari never turns a deny into an allow. A deny and "leave it to the terminal" are always safe.
6. Every allow and every answer asks for Touch ID or the Mac password, inside the hook, before the
   hook prints anything. The dialog names the session and the action from the hook's own input, so
   a fake dashboard or injected key presses cannot approve a tool call. A cancelled or failed check
   means no output. A deny needs no check. On Linux the dashboard offers only deny.

### The hook

1. `install.rs` always registers a second, synchronous hook on `PermissionRequest` only, as the
   hook kind `answer`: `jazari watch answer --hook`. Its `timeout` is the longest allowed wait
   (600 seconds) plus 10 seconds. The async event hook stays as it is. Always registering the hook
   fits the record of installed hooks, and turning the feature on needs no reinstall.
2. While the feature is off, the hook reads only the setting, does not read its input, writes no
   file and prints nothing. So the terminal prompt behaves as without jazari.
3. The hook reads the input: `session_id`, `tool_name`, `tool_input`, `permission_suggestions`.
   For ExitPlanMode it exits at once with no output.
4. The hook connects to the dashboard's Unix socket in jazari's data dir. The socket has mode 600,
   in a folder with mode 700. Before it sends anything, the hook checks the server through the
   kernel's peer credentials (`LOCAL_PEERPID` and `getpeereid` on macOS, `SO_PEERCRED` on Linux):
   - the server runs as the same user;
   - the server is a jazari process;
   - no Claude Code process is among the server's parent processes.
   A dashboard that a session started directly fails the last check. No socket or a failed check
   means that the hook prints nothing, so the terminal decides.
5. The hook sends the request over the socket: the tool, a preview, the questions and the time
   limit. The preview is redacted with `telegram::redact`. No request or answer is written to a
   file, so a session cannot answer by writing a file.
6. The hook waits for one answer line. It checks that the answer fits the request: an offered
   option for a question, and an offered rule for "always allow".
7. The hook stops early when the terminal answered first. The signal is a later hook event from the
   main agent of the same session. Examples are PostToolUse, PreToolUse, Stop and
   UserPromptSubmit. The spike showed that the hook keeps running after a terminal answer, so this
   check is needed.
8. The hook logs `JazariPromptOpened` and `JazariPromptClosed` to the event log. The closing event
   has the outcome (`dashboard`, `desk`, `timeout` or `dropped`) and the answer.
9. The risk check refuses any tool call that names the answer socket or the message inbox, as a
   second layer.
10. Windows has no Unix socket here, so the hook never answers there.

Limit: a process of the same user that starts a real dashboard outside Claude Code's process tree
is not stopped. For example, a session can start a dashboard through tmux, whose server leaves
the session's process tree.

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

1. The dashboard starts the socket server when it opens. A second dashboard does not start one.
2. The footer shows `r answer (N waiting)` first, while prompts wait.
3. Key `r` opens the answer box for the selected session. For a permission prompt the keys are `y`
   (allow), `A` (always allow, only when a suggestion exists) and `n` (deny). For a question, `1`-`9`
   chooses an option, one question after another. In a multi-select question, `1`-`9` marks options
   and Enter sends them. Esc leaves the prompt to the terminal.
4. The box closes by itself when the prompt was answered at the terminal or timed out.
5. `t` (tell) stays next to `r`, so that the founder can answer and send messages to many sessions
   from one place.

### Telegram

Telegram is not part of Part A. Telegram for manual sessions is a later follow-up, only if the
founder opts in.

### The record

Every outcome is written to the event log, with the session, the tool, the preview, the answer and
the outcome.

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
| `src/install.rs` | The synchronous `PermissionRequest` hook, kind `answer`, always registered |
| `src/answer.rs` (new) | The hook, the socket server and its peer check, the output JSON |
| `src/answer_view.rs` (new) | The dashboard's answer box |
| `src/risk.rs` | The rule that refuses tool calls naming the socket or the inbox |
| settings (through `settings::get` and `set`) | `answer_prompts` (off), `answer_wait_secs` (120) |
| `src/app.rs`, `src/ui.rs` | Key `r`, the server's poll, and the footer hint |
| `src/jazari/routine.rs` (new) | The deterministic check for Part B |
| `src/jazari/supervisor.rs` | Part B: check before the founder card |
| `src/jazari/store.rs` | The per-product auto-answer setting |
| `README.md`, `CHANGELOG.md`, `assets/SKILL.md` | Keys, commands and the new setting |

Tests never start Claude Code, never open Telegram and never touch the real data dir. The hook's
clock and file reads are injected, like `share::Desktop::for_tests()`.

## Order of work

1. This plan and the test note, as a doc PR.
2. Part A: the hook, the socket and the dashboard view, as one PR.
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
