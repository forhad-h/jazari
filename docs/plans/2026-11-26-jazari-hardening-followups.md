# jazari hardening follow-ups

These gaps were found in the review of jazari's first release. They are documented in the README's
"Limits to know" and do not block the merge. Each item says what the gap is, why it matters, and
the fix.

## 1. Network egress from worker containers

- **Gap:** containers have open network access. The approval gates for deploy, spend and contact
  are rules in the workers' instructions, not technical controls. A worker that ignores them can
  call production APIs, payment providers or mail services directly, or exfiltrate code.
- **Fix:**
  - Run the containers on an internal Docker network whose only route out is an egress proxy, such
    as Squid or a small CONNECT proxy in marsad.
  - Allow only the Anthropic API, package registries (npm, PyPI, crates.io, the Debian mirrors),
    GitHub and the hosts listed per product in `company/EGRESS`. A change to that file gets full
    review, like `checks.sh`.
  - Log every denied connection to the audit record, and tell the founder once per host.
- **Done when:** a test container can't reach a host outside the list, and a worker task that tries
  to reach one shows the denial in `marsad jazari audit`.

## 2. Tamper-evident audit record

- **Gap:** the hash chain has no key. It shows an edited or removed entry in the middle, but not
  entries cut from the end, or a whole file rewritten by someone with local access.
- **Done:** replaced by the off-machine head anchor. An HMAC key was dropped, because any program
  that runs as the same user can read a Keychain item that `security` created, without a prompt.
  - The supervisor sends the founder the entry count and the head (the last hash) on Telegram when
    it starts, when its loop ends and every 50 entries, and keeps a copy in `audit-head.json`.
  - `jazari build audit` fails when the record has fewer entries than that copy, or a different
    hash at that entry. It prints the exact Telegram line to compare with the founder's chat.

## 3. Pairing brute force

- **Gap:** anyone who knows the bot's username can send up to 5 guesses during a pairing window.
  For a 6-digit code that is a 1 in 200,000 chance per window, small but not zero.
- **Fix:**
  - Make the code 8 or more random alphanumeric characters.
  - Allow one wrong guess per chat id, then ignore that chat for the rest of the window.
  - Tell the founder in the terminal how many wrong guesses arrived.
- **Done when:** a second guess from the same chat is ignored, and the code space is large enough
  that 5 guesses are negligible.

## 4. Message text visible in the process list

- **Gap:** Telegram message text goes to `curl` as `--data-urlencode` arguments, so other local
  processes can read it in `ps` while curl runs. The bot token is already sent on stdin.
- **Fix:** send the whole request on stdin. That means the form body in the `-K -` config
  (`data-urlencode = "..."` lines), or JSON with `--data @-`. Then nothing but fixed flags appears
  in argv.
- **Done when:** a test asserts that the curl argv holds only fixed flags.

## Also worth doing

- Run marsad's own risk hook inside the container, so a worker's risky commands are checked even
  in bypass mode.
- A launchd unit for the supervisor and the bridge, so they survive a reboot.
- Bump the Claude Code version that `assets/jazari/Dockerfile` pins (`CLAUDE_CODE_VERSION`, now
  2.1.332) from time to time. Workers stay on that version until someone changes it.
