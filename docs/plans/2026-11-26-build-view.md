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

The input box reuses `TextBox` from `src/prompt_view.rs`. Its `of`, `insert` and `on_key` functions
become `pub(crate)`. No other change to that file.

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

## Slow work runs on a thread

The view owns one worker thread with its own `Store` handle (`jazari::open()`):

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
| `src/build_view.rs` (new) | State, keys, the worker thread, the snapshot, the actions |
| `src/build_ui.rs` (new) | Draws the panes, the details, the input box and the status line |
| `src/app.rs` | A `View::Build` variant, key `B` in the List view, dispatch to `build_view`, the channel read in `tick()`. A few lines only. |
| `src/ui.rs` | One match arm that calls `build_ui::draw`, and the `B` footer hint in the List view |
| `src/prompt_view.rs` | `TextBox::of`, `insert` and `on_key` become `pub(crate)` |
| `src/jazari/mod.rs` | `open()` becomes `pub(crate)`. The heartbeat thread in `run_supervisor`. |
| `README.md`, `CHANGELOG.md` | The `B` key, a "Build view" section, one line under `## Unreleased` |

`assets/SKILL.md` does not change, because no CLI command changes.

## Order of work

Other sessions have claimed some of these files on the agent board. The session that builds the
answer view holds `src/app.rs` and `src/ui.rs` now. The session that builds `jazari claude` waits
for the same two files. The session that hardens Telegram holds `src/jazari/mod.rs`.

1. `build_view.rs`, `build_ui.rs` and the `TextBox` change, with tests. Nobody else has claimed
   these files.
2. When the claim on `src/jazari/mod.rs` is released: `open()` and the heartbeat.
3. When the answer view's PR is merged, in the order agreed with the `jazari claude` session: the
   hooks in `app.rs` and `ui.rs`.
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

## Open questions for the review

- Is `B` the right key, or should the Build view be a tab next to Board and Usage?
- Is 2 minutes the right age for the heartbeat?
- Should the List view also show one line with the count of open build questions? An example is
  "2 build questions wait for you". Then the founder sees them without opening the Build view.
