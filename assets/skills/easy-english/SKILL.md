---
name: easy-english
description: "Write technical English that every reader understands on the first read, whatever their first language. Load this skill BEFORE you write or change prose in any of these: a commit message or PR text (before git commit or gh pr create), a code comment of two or more lines, a .md file (docs, README, plan, runbook), an error, log or screen message, UI text, and any chat reply longer than three sentences (explanations, questions, final reports). Also load it when asked to make text simpler, clearer or easier to read. Do not load it for edits that add no prose (code, config, renames, test names) or for text in a language other than English."
---

# Easy English

## The reader

The reader is a skilled software developer or staff member. Many readers learned English
as a second or third language and read it at about level B1–B2. They know common words and
standard technical terms, but they read slowly. Some use a translation tool. Readers whose
first language is English gain too: plain sentences are faster to read and harder to
misread, especially when the reader is busy or tired. All of them act on what they read,
and a misread step can break production.

Write for the reader who finds English hardest. Then the text works for everyone.

**Goal:** the reader understands each sentence correctly the first time. They do not need
a dictionary. And the text keeps every fact and keeps the exact meaning.

**Write for this reader every time, not only when asked.** The user should never need to
say "make it simpler".

## Why the first draft fails this reader

Claude's normal style packs a lot into each sentence. Research on model output shows that
models use these shapes far more often than people do. Do not use them:

<!-- easy-english:off -->
| Habit | Example | Write instead |
|---|---|---|
| Invented labels | "the check fails closed", "could-not-ask" | the full idea: "if the check gets no answer, it stops the release" |
| Nouns made from verbs | "Validation of the token is performed by the server." | "The server validates the token." |
| -ing and -ed phrases | "Having checked the tag, the job continues, skipping the deploy." | "The job checks the tag. Then it continues, but it does not deploy." |
| Noun stacks | "the release deploy gate status lookup" | "the step that looks up the status of the release" |
| Asides | dashes (—), long brackets, semicolons | a separate sentence |
| Contrast frames | "This is not a bug but a design choice." | "This behaviour is intended." |
| Figures of speech | "load-bearing", "bites", "the disease", "for good" | the plain fact |
| Compressed logic | "A gate that read unknown as fine would be worse than no gate." | each step of the reasoning as its own sentence, joined with "because" or "so" |
| Rare or fancy words | "salient", "orthogonal", "moot" | the common word |
<!-- easy-english:on -->

## How to write

1. **Before you write, answer two questions.** Who reads this? What must they know or do
   after they read it? Put that answer in the first sentence. (For a change to part of a
   text, do not add a new opening sentence.)
2. **Write with the rules below.**
3. **Run the checker on every text of two sentences or more**, including commit messages
   and code comments. Short texts are part of the problem too.

   ```bash
   python3 ~/.claude/skills/easy-english/scripts/check.py FILE        # a file
   python3 ~/.claude/skills/easy-english/scripts/check.py <<'EOF'     # a draft
   text here
   EOF
   ```

   - The checker needs Python 3. On Windows the command is often `py` or `python`
     instead of `python3`. If no Python 3 is installed, skip the checker. Then apply the
     rules by hand, and do step 4 with extra care.

   - It reads Markdown and code comments (`//`, `/* */`, JSDoc). It skips code blocks,
     inline code and YAML frontmatter.
   - **Findings** must be fixed or have a clear reason to stay. **Notes** are hints: read
     each one and fix it when the hint is right.
   - It also lists the words outside a B2 word list. If more than 5 words in 100 are
     unknown, the reader will struggle (research: 95% known words is the minimum). Replace
     each unknown word, or explain it the first time. Real project terms go in
     `.easy-english-words.txt` at the project root, one per line.
   - A clean result does not prove the text is clear. The checker cannot see meaning.
4. **Read it once as the reader.** For each sentence ask: "Would a word-by-word
   translation into another language keep the meaning?" If not, rewrite it.

## The rules

### Sentences

- **One main idea per sentence.** If a sentence holds three ideas, split it into two or
  three sentences.
- **When you split, keep the link word.** "The check failed. So the release stopped." is
  better than "The check failed. The release stopped." Words like "because", "so", "but",
  "then" and "if" show the reader how the ideas connect. Do not cut them.
- **Length: about 10 to 15 words on average.** Over 20 words, split the sentence. Over 25
  words is always too long (ASD-STE100, Simplified Technical English). Do not go below
  about 10 on average: a row of 5-word sentences is choppy and loses the links between
  ideas.
- **Normal word order.** Subject, then verb, then object, close together. Do not put a long
  phrase before the subject or between the subject and the verb. Exception: put a
  condition first. "If the check fails, stop."
- **Every action has its own subject and verb.** Do not start or end a sentence with an
  -ing or -ed phrase.
- **Use verbs, not nouns made from verbs.** "We migrate the data", not "The migration of the
  data is done".
- **Active voice when you know the actor.** "The script deletes the file." If the source
  does not say who does it, keep the passive. Never invent an actor.
- **Write full clauses.** Keep "that", "who" and "which": "the file that the script
  created", not "the file the script created" or "the file created by the script".
  (Many languages mark such a clause clearly. The short English form gives the reader no
  mark.)
- **Keep "a" and "the", but do not let them carry the meaning alone.** Many languages
  have no such words, so readers miss the difference between "a check" and "the check".
  Write "the same check", "another check", "the first check".
- **Clear pronouns.** If "it", "they" or "this" could mean two things, repeat the noun.
  Do not use "This" alone: "This change", "This error".
- **Say what something is, not only what it is not.** Avoid "not X but Y" and "X, not Y"
  frames. A short "Do not …" for a rule or a warning is fine. Never use a double negative.
- **One hedge at most.** "might" is enough. Not "might possibly in some cases".

### Words

- **Use the common word.** See the table at the end.
- **One meaning per small word.**
  - "since" only for time. For a reason, write "because".
  - "as" only for comparison ("as fast as"). For a reason, write "because".
  - "once" only for "one time". For a sequence, write "after".
  - "while" only for time. For a contrast, write "but" or "although".
  - "may": write "can" (it is possible / allowed) or "might" (perhaps).
- **One term for one thing.** If you call it "the check", do not call it "the gate",
  "the guard" and "the probe" later. Readers think a new word means a new thing. When you
  change one part of a file, use the term that the rest of the file uses. If that term is
  project slang, give the plain word once: "the production server (the box)".
- **Explain each new term the first time.** Standard technical terms are fine: database,
  migration, commit, branch, API, deploy, token, hook, boolean, cache, timeout, fallback,
  poll, header. Project slang is not: "the box", "green", "red", "the gate", "Stage B".
- **Internal IDs need their meaning.** "OPS-209 (production runs on Vercel only)". If you
  do not know what an ID means, look it up. If you cannot find it, put the ID at the end
  as a reference ("… (see OPS-209)") and do not invent a meaning.
- **No idioms, slang or cultural references.**
- **Avoid phrasal verbs** (verb + small word, such as "carry out", "figure out", "point
  at"): their meaning is not the meaning of their parts. Standard technical ones are fine:
  log in, sign in, set up, back up, check out, roll back, look up, find out.
- **Use words in their common meaning.** "fire" (write "run"), "land" ("be merged"),
  "surface" ("show"), "cut a release" ("create a release"), "stale" ("out of date").
- **No Latin short forms:** "e.g." → "for example", "i.e." → "that is", "etc." → name the
  items, "via" → "through" or "with", "vs." → "compared with".
- **Exact numbers, dates and units.** "2026-11-25", not "last Tuesday" or "11/12".
  "30 seconds", not "half a minute".

### Shape

- **Steps go in a numbered list.** One action per step. Commands go in code blocks.
- **Three or more parallel items become a list or a table.**
- **Short paragraphs:** at most 5 sentences.
- **Headings say what the section is about**, in plain words.
- **Do not write "above" or "below".** Name the section.
- **Fewer ideas, full words for each idea.** Cut sentences that do not help the reader act
  or understand. But do not save words by compressing an idea. Easy text is often 20% to
  40% longer than the dense version, and that is expected.

## Fit the place

- **Commit subject:** at most 72 characters, no full stop. Here a short subject matters more
  than the small helper words.
- **Commit body and terminal text:** plain text. Use "-" lists. No tables, no bold.
- **One-line messages (log lines, error strings):** no lists. One or two short sentences.
  Say what happened, then what the reader should do.
- **Code comments:** keep the comment marks, the line width and tags such as `@param`.
  Say why the code does something. The code already shows what it does.
- **Chat replies:** answer first, then the reason. Questions to the user describe each
  option by what changes for real people, with a concrete example.
- **Emphasis:** at most one warning mark in a section. Do not write words in ALL CAPS for
  emphasis. Make at most one phrase bold.

## Keep the meaning exact

Simple words must not change the meaning. A blind review of rewrites found these errors
more often than hard words. Check each one before you finish:

1. **Add no new facts.** Do not add a noun, an adjective or a reason that the source does
   not have, unless you checked it in the code. (A rewrite called the PM2 server "the
   production server". Production runs on Vercel, so that was wrong.)
2. **The new word must not already mean something else in this project.** Before you
   replace a term, search the file and the repository for the new word. "test", "rule",
   "close", "limit" and "answer" often have a fixed meaning already. If every plain word
   is taken, keep the exact term and explain it once: "the predicate (the yes/no test in
   this function)".
3. **Keep names that people search for.** Rule titles, IDs, check names, error codes and
   defined terms stay word for word. Put the plain meaning after them.
4. **Keep the scope words and the tense.** List every "only", "any", "all", "every",
   "never", "anywhere", "the one", and every number in the source. Make sure that each
   one is in the rewrite with the same meaning. Keep past events in the past tense, so that
   a fixed bug does not read as a current bug.
5. **No padding.** Do not say the same thing twice in different words. A rewrite is usually
   20% to 40% longer than a dense source. If it is more than 50% longer, look for
   repeated sentences and cut them.

## When you rewrite existing text

- **Do not guess a meaning.** If you do not understand a compressed phrase, read the code or
  the linked text first. If you still do not know, keep the phrase and tell the user.
- **Some coined labels are names.** Other text can refer to a rule, a section or a check
  by its label. Keep that label once, in quotes, and give its meaning after it. Example:
  the rule "presence is not correctness" means that a check must test the same rule as the
  code that uses the value.
- **A chain with arrows** (`a → b → c`) becomes a numbered list with a verb in each step.
- **Keep everything that is not prose exactly as it is:** code, commands, paths,
  identifiers, error codes, link targets, and text quoted from a program or a person. To
  stop the checker from flagging a quote, put it between `<!-- easy-english:off -->` and
  `<!-- easy-english:on -->`.
- **Do not remove a fact, a number, a limit or a warning** to make the text shorter.
  Simpler means clearer, not less true.
- **Do not make it childish.** The readers are skilled professionals. Use simple words but
  keep the full meaning.
- **When you edit someone's own writing,** change only the sentences that break a rule.

## Before and after

<!-- easy-english:off -->

> Before: 🔴 The gate FAILS CLOSED, and that is the whole point — a gate that read
> "unknown" as "fine" would be worse than no gate.
>
> After: ⚠️ If the check cannot find out whether CI passed, it stops the release. This
> behaviour is intended. A check that lets a release continue without an answer is
> dangerous, because people trust the check and stop checking by hand.

> Before: Presence is not correctness: a check must assert the same predicate its
> consumer enforces.
>
> After: A check must test the same rule as the code that uses the value. It is not
> enough to check that the value exists, because a value can exist and still be wrong.

> Before: `// bail early if the stamp's stale so we don't clobber the newer one`
>
> After: `// Stop here if the saved version is out of date, so that we do not overwrite the newer version.`

> Before (commit): `fix: gate now reads blockedReason instead of the sticky bool`
>
> After (commit): `fix: show the real reason why a payment is held`

<!-- easy-english:on -->

## Common word table

<!-- easy-english:off -->

| Instead of | Write |
|---|---|
| utilize, leverage, employ | use |
| prior to | before |
| subsequently | then, later |
| in order to | to |
| facilitate | help, make possible |
| mitigate | reduce |
| commence, initiate, kick off, spin up | start |
| terminate, tear down | stop, remove |
| sufficient | enough |
| approximately | about |
| retrieve | get |
| aforementioned | this, that (name it) |
| thereby, whereby | so, so that |
| notwithstanding, albeit | although |
| figure out | find, calculate |
| carry out | do, run |
| point at, point to | refer to, name |
| wire up | connect |
| roll out | release |
| hold off | wait |
| going forward | from now on |
| for good | permanently |
| stale | out of date |
| brittle | breaks easily |
| load-bearing | important, needed |
| no-op | does nothing |
| sanity check | quick check |
| under the hood | inside, internally |
| out of the box | by default |
| in flight | running, in progress |
| moot | no longer matters |
| caveat | limit, warning |
| canonical | main, standard |
| orthogonal | unrelated |
| idempotent | safe to run again (it gives the same result) |
| fail closed (in new text) | stop when there is no answer |
| fail open (in new text) | continue when there is no answer |

<!-- easy-english:on -->

## Sources

<!-- easy-english:off -->
Rules come from: ASD-STE100 Simplified Technical English; Microsoft Writing Style Guide,
"Global communications"; Google developer documentation style guide, "Write for a global
audience"; US Federal Plain Language Guidelines; UK Home Office, "Designing for people with
limited English"; Kohl, *The Global English Style Guide*; Simple English Wikipedia;
Schmitt et al. 2011 (95%–98% known words); Reinhart et al. 2024, arXiv 2410.16107 (models
overuse -ing clauses and nouns made from verbs); studies on link words and on how models
reach a reading level (arXiv 2406.03030, 2409.20246). Word lists: see `data/SOURCES.md`.
<!-- easy-english:on -->
