# A handoff picks the prompts and skills for the new session

Status: plan, not built. It needs a review before any code is written.

## Goal

After a handoff, jazari opens a new terminal and starts `claude` with the handoff text
(`resume` in `src/handoff.rs`). The new session gets no prompts and no skills from the founder's
library, so it loses, for example, the coordination rules or Easy English that the old session
had. When the work changes (for example from coding to UI tests in Chrome), nothing adds the
prompt or skill that the next step needs.

From now on, the new session starts with:

1. the prompts and skills of the old session;
2. the founder's ticked defaults;
3. up to 3 extra prompts or skills that a small model picks from the library, for the next step
   in the handoff.

The dashboard and the new session's first message show the set, with a short reason for each
extra.

## How jazari knows the old session's set

The old session's process was started with
`claude --append-system-prompt-file <data dir>/launch/<role>-<keys>-<hash>.md`. The file name holds
the prompt and skill keys (for example `session-coordination+output-hygiene+easy-english-0123abcd`).

`jazari build handoff` runs inside the old session. It finds the old session's Claude Code
process (this is already done to close it) and reads its command line. It takes the keys from the
launch file's name and stores them in the handoff record as `inherited`. A session that started
without a launch file has an empty `inherited` list.

jazari reads the command line with `sysinfo`, which is already a dependency. On Windows, or when
the command line can't be read, `inherited` stays empty, and the first reply says "jazari couldn't
read the last session's prompts, so this session starts with your ticked defaults". A `cfg`
fallback keeps the build working on every system.

A key that is no longer in the library (the founder deleted that prompt) is dropped, and the
first message says so.

## How the model picks the extras

`jazari build handoff resume <id>` runs in the new terminal, so the old session never waits for
the model.

1. jazari prints "Choosing prompts and skills for this session…".
2. It runs the same `claude -p` runner that `src/prompt_gen.rs` uses: the model from the
   `generator_model` setting (Haiku by default), `--no-session-persistence`, and `MARKER_ENV`, so
   the run is not shown or counted as a session.
3. The model gets:
   - the handoff text, cut to its first 6,000 characters;
   - the project's folder name;
   - the library: each prompt's and skill's key, title and one-line summary (never the bodies);
   - the keys that the session has already (inherited and ticked).
4. The model answers with JSON: `{"add": [{"key": "...", "reason": "..."}]}`.
5. jazari checks the answer. It keeps at most 3 keys that are in the library and not in the set
   yet, and it cuts each reason to 80 characters. It never removes a key.
6. If the run fails, takes longer than 20 seconds, or returns JSON that jazari can't read, jazari
   starts with the inherited and ticked set only, and prints why.

`jazari build handoff resume <id> --no-pick` skips the model. The setting `handoff_pick` (on by
default) turns the pick off for every handoff: `jazari build handoff --pick off`. jazari records
each pick in the actions log (`jazari watch actions`): the keys that it added and the reasons, or
why it added none.

### Why the model's answer is limited

The old session wrote the handoff text, and that text can contain words from tool output, for
example a web page or a file. So the text can steer the pick. This is acceptable only because the
pick can do so little:

- it can add only keys that are in the founder's library;
- it adds at most 3 keys;
- it adds only prompts and skills, and it never removes a key from the old set or the defaults.

jazari checks the model's JSON strictly: one object with one `add` list, each item with a `key`
and a `reason` that are strings. Any other shape counts as a failed run.

## How the session starts

jazari merges the chosen prompts into one launch file. The library already does this for the
dashboard's launch commands (`src/rules.rs`). It installs the chosen skills into
`~/.claude/skills/` with `skills::provide`. Then it runs:

```sh
claude --session-id <new uuid> --append-system-prompt-file <launch file> "<first line + handoff text>"
```

- jazari makes a new random (v4) UUID for each start.
- `--session-id` gives the new session an id that jazari knows before the session starts. So the
  dashboard can show the set on the right row from the first second.
- The first line asks Claude to begin its first reply with the set, for example:

  > jazari started this session with: coordination, output-hygiene (from the last session);
  > easy-english (your default); claude-in-chrome-testing (added: the next step tests the UI in
  > Chrome).

## What the founder sees

- The new session's first reply lists the set, with a reason for each extra.
- The dashboard's session row shows the set. The details view shows each reason.
- The founder can change the set with the existing prompt picker. A change applies to the next
  session, because a running session keeps the system prompt that it started with.

## Rules

- Only keys from the founder's library can be added. The model can't write prompt text.
- Only titles and summaries of the library go to the model, never the bodies of prompts.
- The model never removes a prompt or skill.
- The handoff never waits for the model: the pick runs in the new terminal.
- Nothing goes to Telegram. This is no decision for the founder (board Decision on Telegram
  messages).

## Parts

1. `src/handoff.rs`: store `inherited` in the record. In `resume`, run the pick, build the launch
   file, install the skills, and start `claude` with `--session-id`.
2. `src/handoff_pick.rs` (new): the model input, the JSON check, the 20-second limit and the
   fallback. The `claude -p` call is an injectable function, as in `src/prompt_gen.rs`, so tests
   never run `claude -p`.
3. `src/rules.rs`: read the keys from a launch file name (the opposite of the existing naming).
4. The dashboard (`src/ui.rs`, `src/model.rs`): show the set on the session row and the reasons
   in the details view. jazari logs a `JazariLaunchSet` event with the session id, the keys and
   the reasons.
5. Docs: README (the handoff section), CHANGELOG, `assets/SKILL.md`, the footer key hints if a key
   is added.

## Tests

- Keys from a launch file name, including a file that jazari didn't write (no keys).
- The JSON check: an unknown key, a key that is already in the set, more than 3 keys, a long
  reason, and text that isn't JSON.
- The fallback: a run that fails, and a run that takes longer than 20 seconds.
- The first line lists inherited keys, ticked keys and extras with reasons.
- The model input holds titles and summaries, never prompt bodies.

## Checked by hand before the code relies on it

- `claude --session-id <uuid> --append-system-prompt-file <file> "<text>"` works in the installed
  Claude Code version: the session gets that id and reads the file.

## Not checked by tests

- How well the model picks. This needs a few real handoffs, read by a person.
