#!/usr/bin/env python3
"""Find sentences that are hard to read for any reader of technical English.

Usage:
    check.py FILE [FILE ...]
    some-command | check.py

It reads plain text, Markdown and code comments (//, /* */, JSDoc). It skips YAML
frontmatter, fenced code blocks, inline `code`, and text between
<!-- easy-english:off --> and <!-- easy-english:on -->.

Findings change the exit code: 0 when there are none, 1 when there are some.
Notes are hints only; read them, but they do not change the exit code.
It finds mechanical problems only. It cannot judge whether the meaning is clear.

Word check: a word is "known" when it is in the CEFR-J list (levels A1-B2), the NGSL,
data/tech-words.txt, or a project word file. A project word file is
.easy-english-words.txt (one word per line, # for comments) in the current folder or
any parent folder, or a file given with --words FILE.
"""
import csv
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
DATA = os.path.join(HERE, "..", "data")
TECH = set()
MAX_UNKNOWN_SHARE = 0.05    # research: 95% known words is the minimum for understanding

FUNCTION_WORDS = set("""a an the and or but so if then than that this these those it its they them their
there here we you he she his her our your my me us of in on at to for from by with without about into
onto over under after before between through during until while when where which who whom whose what
why how not no nor is are was were be been being am do does did done have has had can could will would
shall should may might must as also only just very more most less least each every all any some such
other same own both either neither one two""".split())


def _read_word_file(path, known):
    try:
        with open(path, encoding="utf-8") as f:
            for line in f:
                line = line.split("#", 1)[0].strip().lower()
                if line:
                    known.add(line)
    except OSError:
        pass


def load_vocabulary(extra_files):
    known, nouns = set(), set()
    try:
        with open(os.path.join(DATA, "cefrj-vocabulary.csv"), encoding="utf-8") as f:
            for row in csv.DictReader(f):
                for w in row["headword"].lower().split("/"):
                    w = w.strip()
                    known.add(w)
                    if "noun" in row["pos"]:
                        nouns.add(w)
        with open(os.path.join(DATA, "ngsl-forms.csv"), encoding="utf-8") as f:
            for line in f:
                known.update(x.strip().lower() for x in line.split(",") if x.strip())
    except OSError:
        return None, None       # no word lists: skip the word check
    _read_word_file(os.path.join(DATA, "tech-words.txt"), TECH)
    known |= TECH
    d = os.getcwd()
    while True:
        _read_word_file(os.path.join(d, ".easy-english-words.txt"), known)
        parent = os.path.dirname(d)
        if parent == d:
            break
        d = parent
    for p in extra_files:
        _read_word_file(p, known)
    return known, nouns


def base_forms(w):
    """Possible dictionary forms of an English word: checks -> check, retried -> retry."""
    out = [w]
    for suf, rep in (("ies", "y"), ("ied", "y"), ("ier", "y"), ("iest", "y"), ("ily", "y"),
                     ("es", ""), ("s", ""), ("ed", ""), ("ed", "e"), ("ing", ""), ("ing", "e"),
                     ("ly", ""), ("er", ""), ("er", "e"), ("est", ""), ("ment", ""), ("ness", ""),
                     ("able", ""), ("able", "e"), ("ation", "e"), ("ation", "")):
        if w.endswith(suf) and len(w) - len(suf) >= 3:
            stem = w[: -len(suf)] + rep
            out.append(stem)
            if len(stem) > 3 and stem[-1] == stem[-2]:      # stopped -> stopp -> stop
                out.append(stem[:-1])
    if w.startswith("re") and len(w) > 5:                    # rerun, retry, rewrite
        out.append(w[2:])
    if w.startswith("un") and len(w) > 5:                    # unset, unknown
        out.append(w[2:])
    return out


def is_known(w, known):
    w = w.lower().strip("'’")
    if w in known:
        return True
    if "-" in w:
        return all(is_known(p, known) for p in w.split("-") if p)
    return any(b in known for b in base_forms(w))

MAX_WORDS = 20      # split a sentence above this
HARD_MAX = 25       # always too long above this

# word or phrase -> plain replacement
HARD_WORDS = {
    "utilize": "use", "utilise": "use", "leverage": "use", "employ": "use",
    "prior to": "before", "subsequently": "then / later",
    "in order to": "to", "facilitate": "help", "mitigate": "reduce",
    "commence": "start", "initiate": "start", "terminate": "stop",
    "sufficient": "enough", "approximately": "about", "retrieve": "get", "aforementioned": "this / that",
    "henceforth": "from now on", "thereby": "so", "whereby": "by which / so that",
    "notwithstanding": "even though", "albeit": "although", "moot": "no longer matters",
    "caveat": "limit / warning", "salient": "important", "ostensibly": "seems to",
    "vis-à-vis": "about / compared with", "ergo": "so", "nuance": "detail",
    "stale": "out of date", "brittle": "breaks easily", "load-bearing": "important",
    "no-op": "does nothing", "sanity check": "quick check", "idempotent": "safe to run again",
    "orthogonal": "unrelated", "canonical": "standard / main", "footgun": "easy mistake",
    "gotcha": "trap / surprise", "heuristic": "rule of thumb -> simple rule",
    "fail-closed": "stops when it cannot get an answer", "fail-open": "continues when it cannot get an answer",
    "fails closed": "stops when it cannot get an answer", "fails open": "continues when it cannot get an answer",
    "e.g.": "for example", "i.e.": "that is", "etc.": "and so on / name the items", "via": "through / with",
    "vs.": "compared with", "unambiguously": "always / clearly",
}
# these are flagged only in a figurative pattern, not as plain words
CONTEXT_IDIOMS = {
    r"\b(?:landed|lands|land) (?:on|in) (?:develop|main|the trunk|master|production)\b": "'land' -> 'be merged'",
    r"\b(?:bit|bites|bite) (?:us|you|them|me|here|again)\b": "'bite' -> 'cause a problem'",
    r"\bon top of (?:that|this|which|it)\b": "'on top of that' -> 'also'",
}
# a hard word is fine in front of these (standard technical terms)
TECH_AFTER = {"canonical": r"(?:url|name|form|link|tag)", "terminate": r"(?:the )?(?:tls|ssl)", "stale": r"-while-"}

IDIOMS = [
    "under the hood", "out of the box", "in flight", "for good", "going forward",
    "moving forward", "at the end of the day", "on the fly", "rule of thumb",
    "body count", "the whole point", "round up", "rounding up", "in a nutshell",
    "low-hanging fruit", "boil down", "boils down", "the bottom line",
    "take it from here", "the ball", "heads up", "ballpark", "hit the ground",
    "red flag", "the elephant", "by the same token", "for the want of", "spoke and lied",
    "the disease", "a smell",
    "goes on", "where things stand", "the way back",
]

PHRASAL = [
    "carry out", "figure out", "point at", "point to", "wire up", "roll out",
    "hold off", "kick off", "spin up", "tear down", "come up with", "run into",
    "look into", "put off", "turn out", "end up", "bubble up", "fall back",
    "set aside", "pick up", "go over", "deal with", "get rid of", "make up",
    "show up", "back off", "trip up", "call out", "lay out",
]
# phrasal verbs that are normal in technical English and stay
ALLOWED_PHRASAL = {"log in", "sign in", "set up", "back up", "check out", "roll back",
                   "log out", "sign out", "look up"}

SPECIAL_MEANING = {
    r"\bfires?\b": "'fire' -> 'run'",
    r"\bsurfaces?\b(?= (?:the|a|an|it|this|that|them))": "'surface' (verb) -> 'show'",
    r"\bcut (?:a|the) (?:release|tag)\b": "'cut a release' -> 'create a release'",
}


def strip_markdown(text):
    out = []
    in_fence = False
    in_comment = False          # inside a /* ... */ block
    off = False                 # between <!-- easy-english:off --> and :on
    lines = text.splitlines()
    front = bool(lines) and lines[0].strip() == "---"
    for i, line in enumerate(lines):
        if front:               # YAML frontmatter is not prose
            out.append("")
            if i > 0 and line.strip() == "---":
                front = False
            continue
        if "easy-english:off" in line or "easy-english:on" in line:
            off = "easy-english:off" in line
            out.append("")
            continue
        if off:
            out.append("")
            continue
        # code comment markers: //, /** ... */, and the leading * inside a block comment
        if re.match(r"^\s*/\*", line):
            in_comment = True
        if in_comment:
            ends = "*/" in line
            line = re.sub(r"^(\s*)(?:/\*+|\*(?!/))\s?", r"\1", line)
            line = re.sub(r"\s*\*+/\s*$", "", line)
            in_comment = not ends
        line = re.sub(r"^(\s*)//+\s?", r"\1", line)
        line = re.sub(r"^(\s*)[•·]\s*", r"\1- ", line)
        if re.match(r"^\s*(```|~~~)", line):
            in_fence = not in_fence
            out.append("")
            continue
        if in_fence:
            out.append("")
            continue
        line = re.sub(r"`[^`]*`", "CODE", line)
        line = re.sub(r"\*\*|__", "", line)                                   # bold marks
        line = re.sub(r"(?<![\w*])\*(?=\S)([^*]+?)(?<=\S)\*(?![\w*])", r"\1", line)  # *italic*
        line = re.sub(r"\]\([^)]*\)", "]", line)          # link targets
        line = re.sub(r"https?://\S+", "URL", line)
        out.append(line)
    return "\n".join(out)


def sentences(text):
    """Yield (line_number, sentence, kind). kind is 'p', 'li', 'q', 'h' or 'cell'."""
    buf, start, kind, li_indent = [], None, None, 0

    def flush():
        nonlocal buf, start
        if buf:
            yield from ((n, x, kind) for n, x in _split(" ".join(buf), start))
        buf, start = [], None

    for n, line in enumerate(text.splitlines(), 1):
        s = line.strip()
        indent = len(line) - len(line.lstrip())
        if not s:
            yield from flush()
            kind = None
            continue
        if re.match(r"^#{1,6}\s", s):
            yield from flush()
            kind = "h"
            yield from ((n, x, "h") for n, x in _split(s.lstrip("#").strip(), n))
            kind = None
            continue
        if s.startswith("|"):
            yield from flush()
            kind = None
            if re.match(r"^\|?[\s:|-]+\|?$", s):        # the |---|---| row
                continue
            for cell in s.strip("|").split("|"):
                yield from ((n, x, "cell") for n, x in _split(cell.strip(), n))
            continue
        m = re.match(r"^(?:[-*+]|\d+[.)])\s+(.*)", s)
        if m:
            yield from flush()
            buf, start, kind, li_indent = [m.group(1)], n, "li", indent
            continue
        if s.startswith(">"):
            body = s.lstrip(">").strip()
            if kind != "q":
                yield from flush()
            kind = "q"
            if not body:
                yield from flush()
                continue
            if start is None:
                start = n
            buf.append(body)
            continue
        if kind == "q":
            yield from flush()
            kind = None
        if kind == "li" and buf:
            # a wrapped list item: indented, or the item's sentence is clearly not finished
            done = buf[-1].rstrip()[-1:] in ".!?:"
            if not (indent > li_indent or done or s[:1].islower()):
                yield from flush()
                kind = "p"
        elif kind != "li":
            kind = "p"
        if start is None:
            start = n
        buf.append(s)
    yield from flush()


ABBR = re.compile(r"\b(e\.g|i\.e|etc|vs|cf|approx|no)\.(?=\s)", re.I)
# sentence end, optional closing quote/bracket, space, optional emoji or symbol, then a start
BOUNDARY = re.compile(r"([.!?][\"'”’)\]]*)\s+(?:[^\w\s\"'“(\[`$<>|/-]+\s+)?(?=[A-Za-z0-9\"'“(\[`$<])")


def _split(chunk, line_no):
    chunk = ABBR.sub(lambda m: m.group(0).replace(".", "\u2024"), chunk)   # protect "e.g."
    for part in BOUNDARY.sub(r"\1\n", chunk).split("\n"):
        part = part.replace("\u2024", ".").strip()
        if part:
            yield line_no, part


# a token that is code even without backticks: path, flag, file name, ${var}, <placeholder>, snake_case, date, version
CODE_TOKEN = re.compile(
    r"^(?:[/~]?[\w.@~-]+(?:/[\w.@~-]+){1,}|[\w.@~-]+(?:/[\w.@~-]+){2,}|[\w.@~-]*/[\w.@~-]+\.\w{1,5}|--?[a-z][\w-]*=?\S*|"
    r"[\w-]+\.(?:sh|mjs|cjs|js|ts|tsx|json|md|yml|yaml|py|sql|env|txt|css|html)|"
    r"\$\{[^}]*\}\S*|<[\w-]+>|\w+_\w+|\d{4}-\d{2}-\d{2}|v?\d+(?:\.\d+)+|CODE\S*)$")


def words(s):
    out = []
    for tok in s.split():
        t = tok.strip("()[]\"'“”,;:.!?")
        if CODE_TOKEN.match(t):
            out.append(t)
        else:
            out += re.findall(r"[A-Za-z][A-Za-z'’-]*|\d+", tok)
    return out


def hard_word_pattern(w):
    """Match the word and its common endings: ensure, ensures, ensured, ensuring."""
    if " " in w or "-" in w or "." in w:
        return r"(?<![\w-])" + re.escape(w) + r"(?![\w-])"
    stem = w[:-1] if w.endswith("e") else w
    return r"\b" + re.escape(stem) + r"(?:e|es|ed|ing|s)?\b"


NOT_UN = (r"\bnot (?:un(?:likely|common|usual|reasonable|important|true|clear|safe|necessary|related|"
          r"like|helpful|expected|known|reliable)|in(?:frequent|significant|correct|valid|complete))\b|"
          r"\bnever fails? to\b|\bcannot fail\b|\bcannot not\b|\bnot without\b")
THIS_VERBS = ("is|was|means|makes|will|does|can|lets|breaks|happens|causes|shows|allows|gives|keeps|"
              "stops|needs|works|has|had|should|must|may|would|could")


NOMINAL = re.compile(r"\b\w{4,}(?:tion|sion|ment|ance|ence|ure)s? of\b")
ING_OPENER = re.compile(r"^(?:\w+ly )?[a-z]+(?:ing|ed)\b[^,.]{0,60},")
ING_TRAIL = re.compile(r", (?!including|according|depending|following|starting|using)[a-z]{3,}ing\b")
PASSIVE = re.compile(r"\b(?:is|are|was|were|be|been|being)\s+(?:\w+ly\s+)?(\w+ed|built|done|made|"
                     r"sent|set|shown|given|taken|written|run|kept|held|found|known|seen)\b(?![^.]*\bby\b)")
TWO_MEANINGS = {
    r"\bsince\b(?! (?:\d|v\d|then|the (?:start|first|last|move|release)|january|february|march|april|"
    r"may|june|july|august|september|october|november|december|yesterday|last|commit|version))":
        "'since' here means 'because'? write 'because' (keep 'since' for time)",
    r"(?:^|, )as (?:it|they|we|you|the|this|that|there)\b(?! (?:\w+ )?(?:as|if|follows|shown|well|long|soon))":
        "'as' meaning 'because'? write 'because'",
    r"^once (?:the|it|you|we|they|a|this|that)\b|, once (?:the|it|you|we|they|a)\b":
        "'once' meaning 'after'? write 'after' (keep 'once' for 'one time')",
    r"\bmay (?!\d)": "'may' is permission or possibility? write 'can' or 'might'",
}
CONTRAST = re.compile(r"\bnot (?:just |only |merely )?\w+(?: \w+){0,4},? but\b|, not (?:a |an |the )?\w+[,.]|\brather than\b")


def check(text, name, vocab=(None, None)):
    known, nouns = vocab
    findings = []
    notes = []          # shown, but they do not change the exit code
    lengths = []
    seen_ids = set()
    total_words, unknown_words = 0, []
    for line, s, kind in sentences(strip_markdown(text)):
        low = s.lower()
        unquoted = re.sub(r"[\"“][^\"”]*[\"”]", " QUOTE ", low)   # do not judge words inside "quotes"
        n = len(words(s))
        if n >= 4:                       # labels and table cells do not count as sentences
            lengths.append(n)
        short = (s[:90] + "…") if len(s) > 90 else s
        if kind == "q" and s[:1] in "\"“":
            continue                     # a quoted passage stays exactly as it is
        if n > HARD_MAX:
            findings.append((line, f"too long ({n} words, max {HARD_MAX}): split it", short))
        elif n > MAX_WORDS:
            findings.append((line, f"long ({n} words): try to split", short))
        if "—" in s or " – " in s or " -- " in s or re.search(r"\w - \w", s):
            findings.append((line, "dash: make the aside its own sentence", short))
        if ";" in unquoted:
            findings.append((line, "semicolon: use two sentences", short))
        if any(len(words(b)) > 6 for b in re.findall(r"\(([^)]*)\)", s)):
            findings.append((line, "long bracket: make it its own sentence", short))
        if len(re.findall(r"→|->|=>", s)) >= 2:
            findings.append((line, "arrow chain: use a numbered list", short))
        for w, rep in HARD_WORDS.items():
            m = re.search(hard_word_pattern(w), unquoted)
            if m and not (w in TECH_AFTER and re.match(r"\s*" + TECH_AFTER[w], unquoted[m.end():])):
                findings.append((line, f"hard word '{w}' -> '{rep}'", short))
        for w in IDIOMS:
            if re.search(r"\b" + re.escape(w) + r"\b", unquoted):
                findings.append((line, f"idiom or figure '{w}': say the plain fact", short))
        for pat, msg in CONTEXT_IDIOMS.items():
            if re.search(pat, unquoted):
                findings.append((line, msg, short))
        for w in PHRASAL:
            verb, particle = w.split(" ", 1)
            pat = r"\b" + verb + r"(?:s|es|ed|ing|t)?\b \b" + re.escape(particle) + r"\b"
            if w not in ALLOWED_PHRASAL and re.search(pat, unquoted):
                findings.append((line, f"phrasal verb '{w}': use one plain verb", short))
        for pat, msg in SPECIAL_MEANING.items():
            if re.search(pat, unquoted) and not re.search(r"\bfire (?:alarm|wall|base)", unquoted):
                findings.append((line, msg, short))
        if re.search(NOT_UN, unquoted):
            findings.append((line, "double negative: say it positively", short))
        if re.match(r"^this\s+(" + THIS_VERBS + r")\b", low):
            findings.append((line, "'This' alone: name the thing (This change, This error)", short))
        if re.search(r"\b(?:above|below)\b(?! \d)", unquoted):
            findings.append((line, "'above' / 'below': name the section", short))
        prose = " ".join(t for t in unquoted.split() if not CODE_TOKEN.match(t.strip("()[]\"',;:.!?")))
        for label in re.findall(r"(?<![\w/.-])[a-z]+(?:-[a-z]+){2,}(?![\w/-])", prose):
            notes.append((line, f"invented label '{label}'? say the full idea", short))
        for m in re.finditer(r"\b[A-Z]{2,6}-\d{1,4}\b", s):
            if m.group(0) not in seen_ids:
                seen_ids.add(m.group(0))
                if not re.match(r"\s*[(:,]", s[m.end():]):
                    notes.append((line, f"'{m.group(0)}' first use: say what it is", short))
        # sentence shapes that models overuse and second-language readers misread
        if ING_OPENER.match(unquoted) and kind != "h":
            findings.append((line, "opens with an -ing/-ed phrase: give the action its own subject and verb", short))
        if ING_TRAIL.search(unquoted):
            notes.append((line, "', ...ing' at the end: make it its own sentence with a subject", short))
        if len(NOMINAL.findall(unquoted)) >= 2:
            notes.append((line, "nouns made from verbs ('validation of'): use the verbs", short))
        if CONTRAST.search(unquoted):
            notes.append((line, "contrast frame ('not X but Y', ', not X'): say what it IS, plainly", short))
        if PASSIVE.search(unquoted):
            notes.append((line, "passive with no actor: name who does it, if the source says", short))
        for pat, msg in TWO_MEANINGS.items():
            if re.search(pat, unquoted):
                notes.append((line, msg, short))
        # word check
        if known is not None and kind != "h":
            toks = [t for t in s.split() if not CODE_TOKEN.match(t.strip("()[]\"'“”,;:.!?"))]
            run = []
            for i, tok in enumerate(toks):
                t = tok.strip("()[]\"'“”,;:.!?*_")
                for w in re.findall(r"[A-Za-z][A-Za-z'’-]*", t):
                    if w.isupper() or (w[0].isupper() and i > 0) or "QUOTE" in w:
                        continue        # names, acronyms and quoted text
                    if re.search(r"[a-z][A-Z]", w):
                        continue        # camelCase is code
                    w = re.sub(r"['’]s$", "", w)
                    total_words += 1
                    if not is_known(w, known):
                        unknown_words.append(w.lower())
                # noun stack: 4+ nouns in a row, no small word or punctuation between them
                lw = t.lower()
                if (re.fullmatch(r"[a-z][a-z-]*", lw) and lw not in FUNCTION_WORDS
                        and not re.search(r"(?:ly|ed)$", lw)
                        and (lw in nouns or any(b in nouns for b in base_forms(lw))
                             or lw in TECH or not is_known(lw, known))
                        and not re.search(r"[,;:.!?)]$", tok)):
                    run.append(lw)
                    if len(run) == 4:
                        notes.append((line, f"noun stack '{' '.join(run)} …': use 'of', 'for' or a verb", short))
                else:
                    run = []
    for line, msg, s in findings:
        print(f"{name}:{line}: {msg}\n    {s}")
    for line, msg, s in notes:
        print(f"{name}:{line}: note: {msg}\n    {s}")
    if lengths:
        avg = sum(lengths) / len(lengths)
        over = sum(1 for x in lengths if x > MAX_WORDS)
        tiny = sum(1 for x in lengths if x < 7)
        note = "  (aim for about 15)" if avg > 17 else ""
        if tiny * 2 > len(lengths) and len(lengths) >= 6:
            note += "  (more than half are under 7 words: join some with 'because', 'so' or 'but')"
        print(f"{name}: {len(lengths)} sentences, average {avg:.1f} words, longest {max(lengths)}, "
              f"{over} over {MAX_WORDS}{note}, {len(findings)} findings, {len(notes)} notes")
    if known is not None and total_words:
        share = len(unknown_words) / total_words
        uniq = sorted(set(unknown_words))
        bad = share > MAX_UNKNOWN_SHARE and total_words >= 40
        print(f"{name}: words outside the B2 word list: {len(unknown_words)} of {total_words} "
              f"({share:.0%}, limit {MAX_UNKNOWN_SHARE:.0%}){'  TOO MANY' if bad else ''}")
        if uniq:
            print("    replace, or explain on first use, or add real project terms to "
                  ".easy-english-words.txt:\n    " + ", ".join(uniq))
        if bad:
            return len(findings) + 1
    return len(findings)


def main():
    args = sys.argv[1:]
    extra = []
    while "--words" in args:
        i = args.index("--words")
        extra.append(args[i + 1])
        del args[i:i + 2]
    vocab = load_vocabulary(extra)
    total = 0
    if args:
        for path in args:
            with open(path, encoding="utf-8") as f:
                total += check(f.read(), path, vocab)
    else:
        total += check(sys.stdin.read(), "<stdin>", vocab)
    sys.exit(1 if total else 0)


if __name__ == "__main__":
    main()
