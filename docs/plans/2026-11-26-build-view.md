# A Build view in the dashboard

Status: plan, not built. It needs a review before any code is written.

## Goal

Today the founder manages jazari's sandbox work (products, tasks, worker questions and approvals)
only with `jazari build ...` commands or from Telegram. The dashboard shows only the sessions on
this machine.

This plan adds a **Build view** to the dashboard. In it, the founder can:

1. See every product and its tasks.
2. Answer a blocked worker's question, and approve or deny a deploy, spend or contact request.
3. Type a new idea or task for a product, or start a new product, in an input box.
4. See whether the supervisor runs, and the last usage reading.

The Build view shows only sandbox work. Sessions that the founder started by hand stay in the
List view. This split follows the board decision of 2026-11-26 16:29. In that decision, Telegram
controls only the sandbox work, and the dashboard controls both kinds of work.

## How this fits with the other plans

- **Answer prompts from jazari** (`2026-11-26-answer-prompts-from-jazari.md`): Part A adds an
  answer view for the prompts of host sessions. It opens with key `a` in the List view. A host
  prompt is a file that a waiting hook reads. A worker question is different: the supervisor
  stores it in the task, in `tasks.question` or `tasks.approval`. So the Build view answers the
  questions of workers, and the answer view answers the prompts of host sessions. Neither view
  shows the other's prompts.
- Part B of the same plan lets jazari answer routine worker questions by itself. The Build view
  shows those tasks with the mark "answered by jazari", so the founder can review the answer.
  Until Part B is built, the mark never appears.
- **Project prompts and `jazari claude`** (`2026-11-26-project-prompts-and-jazari-claude.md`): it
  changes the `c` picker and the session details. It has no part in the Build view.

## What the view shows

At about 182 columns there are two panes. At about 110 columns the same panes are narrower, and
long titles are cut.

| Part | Where | What |
|---|---|---|
| Needs you | top, full width | Every blocked task with a question or an approval, oldest first. One line each: `#id product role: question`. |
| Products | left pane | Name, mode (`normal` or `autonomous`), and the count of running, blocked and queued tasks. |
| Tasks | right pane | The selected product's tasks, in the order of `jazari build tasks`: running and blocked first, then queued, then finished. |
| Status line | one line over the footer | Whether the supervisor runs. The 5-hour and 7-day usage, and whether jazari would start a task now, with the same text as `jazari build limits`. |
| Input box | bottom, when open | One line of text, with a label that says what Enter does. |

`enter` on a task opens its details: the brief, the summary, the branch, and the full question
with its options. A hard-line question shows the line "This choice crosses a hard line. To pick
another option, start the answer with `override:` and give your reason."

## Keys

The List view opens the Build view with `B`. This key is free in the List view today.

| Key | Where | Does |
|---|---|---|
| `j` `k` | everywhere | Move the selection |
| `tab` | panes | Switch between Needs you, Products and Tasks |
| `enter` | a task | Open the task details |
| `a` | a blocked question | Open the input box for an answer |
| `1`–`4` | a blocked question | Pick an offered option. A second press of the same digit sends it. |
| `y` / `d` | an approval | Approve or deny. A second press of the same key sends it. |
| `i` | a product | Input box: an idea for the Chief of Staff to plan |
| `t` | a product | Input box: a task for the `engineer` role, like `jazari build task` |
| `N` | anywhere | Two input boxes: the new product's name, then its idea |
| `m` | a product | Switch the mode between `normal` and `autonomous`, with a second press to confirm |
| `esc` | anywhere | Close the box, the details, or the Build view |

The second press protects against one wrong key press. The status line says what the second press
will do, for example "Press y again to approve the deploy for #42".

The input box reuses `TextBox` from `src/prompt_view.rs`. Its `of` and `on_key` functions become
`pub(crate)`. No other change to that file.

## Reads and writes go through the same code as the CLI

The view never writes SQL. It calls the same functions that the CLI and the Telegram bridge call,
so that each rule stays in one place:

| Action | Function | Rule that stays in one place |
|---|---|---|
| Typed answer | `jazari::answer_task(store, id, text, true)` | A typed answer is a founder signal for the profile, except when it only picks an offered option. A hard-line question refuses an answer without `override:` (`store.reply`). |
| Option picked with a digit | `jazari::answer_task(store, id, label, false)` | A picked option is never a signal, like a Telegram button tap. |
| Approve, deny | `jazari::decide(store, id, approve)` | The fixed reply texts for approve and deny. |
| Idea | `jazari::queue_idea(store, product, text)` | The idea signal and the Chief of Staff brief. |
| Task | `store.add_task` with `personas::resolve("engineer")` | The same fields as `jazari build task`. |
| New product | `jazari::create_product(store, name, idea)` | Name check, repo folder, first commit, first task. |
| Mode | `jazari::set_mode(store, product, mode)` | The mode check. |

Reads use `store.products()`, `store.tasks(product, limit)` and `store.get("limits")`.

The view shows an error from any of these functions in the footer error line, word for word. One
example is a hard-line answer without `override:`. The input box stays open with the text, so the
founder can fix it.

## The presence check

Another session can type keys into the dashboard, for example with `tmux send-keys` or an
`osascript` keystroke. So a key press alone does not prove that the founder is at the machine.
Before some writes, the view asks for a presence check: Touch ID or the Mac password. The
dashboard uses the same check for approvals. The view reuses `presence::available()` and
`presence::confirm(reason)` from `src/presence.rs` (#131). It does not add a second helper.

| Write | Check |
|---|---|
| Approve a deploy, spend or contact request | yes |
| An answer with `override:` (a hard-line question) | yes |
| A typed answer | yes, because it becomes a founder signal for preference learning |
| An option picked with a digit | yes, because it answers for the founder |
| An idea, a task | yes, because an idea becomes a founder signal, and both start paid work |
| A new product | yes |
| A mode change | yes |
| Deny | no |
| Close a box, or `esc` | no |
| Reading and moving around | no |

Rules:

1. The check runs on the worker thread, before the write, so the dashboard keeps drawing while the
   system dialog is open.
2. If the check fails or the founder cancels it, nothing is written. The footer says "Not sent: the
   presence check failed", and the input box keeps its text.
3. On Linux and Windows there is no presence check yet. There the Build view is read-only, and the
   only write is deny. The keys for the other writes show "needs Touch ID or the Mac password; use
   `jazari build ...` in a terminal" in the footer.

Tests:

- The presence check is an injected function, so tests never open the system dialog.
- A failed check writes nothing, for each checked write in the table.
- Deny works with no check, and on a platform with no check.
- On a platform with no check, each checked key writes nothing and shows the footer text.

## Slow work runs on a thread

The view owns one worker thread with its own store handle (`jazari::dashboard::Db`):

1. Every 2 seconds, and after each action, the thread reads a snapshot and sends it through a
   channel. The snapshot holds the products, the tasks, the limits and the supervisor state.
2. The view sends each action to the thread through a second channel. The thread runs it and sends
   back the result text or the error.
3. `tick()` reads both channels. The UI never waits for SQLite or git.

`create_product` runs git and can take a few seconds. The status line shows "Creating <name>…"
until the result comes back.

The supervisor already sent a Telegram card for the same question, and the card stays in the chat.
The founder can tap the card after answering in the dashboard. Then `store.reply` refuses the tap
with "not waiting for you". It does the same today for a second tap. The Build view sends nothing
to Telegram (board decision of 2026-11-26 15:17).

## Is the supervisor running?

The supervisor holds an exclusive lock on `build/supervisor.lock`. The view must not take that
lock, even for a moment. If a supervisor starts in that moment, it stops with "already running".

So the supervisor writes a heartbeat (a time stamp that it renews while it runs):

1. `run_supervisor` in `src/jazari/mod.rs` starts a small thread after it takes the lock.
2. The thread opens its own `Store` and writes `store.put("supervisor:alive", <now>)` every 30
   seconds.
3. When `run_supervisor` returns, it deletes the key.
4. The view shows "running" when the heartbeat is younger than 2 minutes. Otherwise it shows
   "stopped".

The heartbeat runs on its own thread because one supervisor tick can run a whole task. A task can
take much longer than 2 minutes, so a heartbeat once per tick would show "stopped" during a long
task. A supervisor that crashes leaves an old heartbeat, and after 2 minutes the view shows
"stopped".

The view does not start or stop the supervisor. `jazari build run` and `jazari build service`
already do that.

## Files

| File | Change |
|---|---|
| `src/jazari/dashboard.rs` (new) | The snapshot, the actions, the presence check before a write, the heartbeat. It is a child of `jazari`, so it uses the private `store`, `limits` and `personas` modules directly. |
| `src/build_view.rs` (new) | State, keys, the worker thread, the snapshot, the actions |
| `src/build_ui.rs` (new) | Draws the panes, the details, the input box and the status line |
| `src/app.rs` | A `View::Build` variant, key `B` in the List view, dispatch to `build_view`, the channel read in `tick()`. A few lines only. |
| `src/ui.rs` | One match arm that calls `build_ui::draw`, the `B` footer hint and the build question count in the List view |
| `src/prompt_view.rs` | `TextBox::of` and `on_key` become `pub(crate)` |
| `src/jazari/mod.rs` | `pub mod dashboard`. The heartbeat starts and stops in `run_supervisor`. |
| `src/main.rs` | `mod build_ui;` and `mod build_view;` |
| `README.md`, `CHANGELOG.md` | The `B` key, a "Build view" section, one line under `## Unreleased` |

`assets/SKILL.md` does not change, because no CLI command changes.

## Order of work

Other sessions have claimed some of these files on the agent board. The session that builds the
answer view holds `src/app.rs` and `src/ui.rs` now. The session that builds `jazari claude` waits
for the same two files. The session that hardens Telegram holds `src/jazari/mod.rs`.

1. `build_view.rs`, `build_ui.rs` and the `TextBox` change, with tests. Nobody else has claimed
   these files.
2. `dashboard.rs` and the heartbeat in `src/jazari/mod.rs`, in the order agreed with the sessions that
   hold or wait for that file.
3. After the answer view's PR (#131) is merged, add the presence check. Then add the hooks in
   `app.rs` and `ui.rs`, in the order agreed with the `jazari claude` session.
4. README and CHANGELOG, then one PR.

## Tests

The tests use `Store::memory()` and never open the real data folder, Telegram, git or `claude`.
`create_product` is passed in as a function, so a test gets a fake one.

- The Needs you list holds exactly the blocked tasks with a question or an approval, oldest first.
- A typed answer calls `answer_task` with `typed = true`. A digit pick sends the option's label with
  `typed = false`, and adds no signal.
- A hard-line question with an answer that is not the recommended option keeps the box open and
  shows the `override:` error.
- One `y` does nothing, and two `y` presses approve. A `y` and then a `d` do neither.
- An idea for a product queues one `chief-of-staff` task.
- The snapshot channel and the action channel never block `tick()`: a slow fake action leaves the
  view responsive.
- The heartbeat: younger than 2 minutes is "running", older or missing is "stopped".
- No code in `build_view.rs` calls `tell_founder`, `say` or the Telegram bot. The test reads the
  source text, like the supervisor's push test does.

## Verification

1. `cargo test` and `cargo clippy --all-targets`.
2. A throwaway `TestBackend` test at 182×37 and 110×33 with a few products and tasks, deleted after
   the check.
3. With `MARSAD_HOME=<scratch dir>`:
   1. Add a product with `jazari build new`.
   2. Block a task by hand in the scratch database.
   3. Answer, approve and deny it from the Build view.

## Left out on purpose

- Starting or stopping the supervisor from the view.
- Preferences, proposals (`accept`, `ignore`) and `feedback`. They can come later, with the same
  pattern.
- A task role other than `engineer` in the `t` box.
- Protection against a process that runs as the same user. The presence check stops keys that
  another session types into the dashboard. But such a process can still write `jazari.db`
  directly. This is the same limit that jazari has for every file that it owns.
- The "answered by jazari" mark for Part B of the answer plan. It comes with Part B.

## Decisions from the review

- The key is `B`. It must be free in every view and mode, and a test checks this.
- The heartbeat age is 2 minutes.
- The List view shows a small count of open build questions, for example "2 build questions". The
  count is not pushed to Telegram.
- The presence check guards every write that approves or speaks for the founder (see the section
  "The presence check").
