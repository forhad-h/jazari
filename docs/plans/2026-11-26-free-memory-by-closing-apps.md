# Free memory by closing apps, with approval from Telegram

Status: plan, not built. It needs a review before any code is written.

## Goal

jazari often works alone for hours while the founder is away from the desk. Most of the work
runs in terminals. On a machine with little memory, other apps (a browser, chat apps, an editor)
can push the machine into swap until it stops responding.

jazari gets a way to close the apps that the work does not need, but only after the founder says
yes on Telegram. A session can open Chrome again when it needs it.

## What the founder sees

1. Memory reaches the warning level, or a session runs `jazari watch focus`.
2. jazari sends one Telegram message to the paired chat:

   > Memory is 91% used, swap 5.2 GiB. Close these apps to free about 3.1 GB?
   > [Close Google Chrome (1.4 GB)] [Close Slack (0.9 GB)]
   > [Close Spotify (0.5 GB)] [Close Preview (0.3 GB)]
   > [Close all] [Not now]

3. Each app has its own button. A tap on one app closes only that app. jazari then edits the
   same card: the button changes to "Closed" or "Still open". The other buttons stay until the
   request expires.
4. **Close all** closes every listed app except Chrome. Chrome has only its own button, because
   a Chrome tab can hold a call (see "Rules that keep it safe").
5. On **Not now**, or if there is no answer in 10 minutes, jazari closes nothing more. It asks
   again only after 60 minutes, and only if memory is still at the warning level.

Telegram gets only messages that need the founder's decision or action (a board Decision). So the
approval card is the only message that jazari pushes. The results show as edits of that card.
jazari pushes one more message only when an app stays open with unsaved work, because then the
founder has to act.

The card needs the Telegram bridge. If the bridge is not running (no background service and no
bridge started by hand), nobody can answer, so the request expires and closes nothing. Then
`jazari watch status` and the dashboard show "memory is high; the Telegram bridge is not running".

At the desk, `jazari watch focus` works without Telegram. It shows the same list in the
terminal, with each app's memory, and asks which apps to close.

## Opening a closed app again when the work needs it

jazari remembers each app that it closed, in `~/.jazari/claude/focus-closed.json`. It opens such
an app again when a session needs it. jazari opens only apps that it closed itself.

- **Chrome for browser tools.** A session calls a browser tool (any `mcp__claude-in-chrome__*`
  tool) while Chrome is closed. The guard hook starts Chrome with `open -a "Google Chrome"`.
  The extension needs a few seconds to connect after Chrome starts, so the hook refuses this
  one call. The refusal says what to do next: "jazari is opening Google Chrome, which it closed
  to free memory. Wait 15 seconds, then make the same call again." The next call goes through.
- **Any other app.** A session runs `jazari watch focus --reopen "<app>"`. The block message
  and the jazari-usage skill name this command.
- **The founder.** `jazari watch focus --reopen "<app>"`, or the dashboard.
- When memory is still at the warning level, jazari opens the app anyway, because the work
  needs it. This is no decision for the founder, so jazari does not push it to Telegram. It
  records it in the actions log.

The guard hook is registered with no matcher, so it runs before every tool, MCP tools included.
The hook must not wait for Chrome: it starts `open -a "Google Chrome"` as a detached process and
returns the refusal at once.

## Rules that keep it safe

- **Only the paired chat can approve.** The bridge already drops messages from every other chat.
- **Each request has a one-time code.** The buttons carry a random code for that request. A tap
  on an old message, or a second tap, does nothing.
- **The approval covers only the apps in the message.** If the list changes before the tap,
  jazari closes only the apps that were on the list.
- **No answer means no close.**
- **Polite quit only.** On macOS, jazari sends each app the normal Quit request (as ⌘Q does). An
  app with unsaved work asks the founder to save and stays open. jazari never force-kills an app.
- **The app in front is never closed.** jazari leaves out the frontmost app, which is the app the
  founder is using right now.
- **A keep list that jazari never closes:** terminals (Terminal, iTerm2, Warp, Ghostty, WezTerm,
  kitty), Finder, the Dock and other system processes, and Claude. Call apps are on it too:
  zoom.us, Microsoft Teams and FaceTime. So is every app that runs a Claude Code session as a child
  process. The founder can add apps to this list.
- **Slack is not on the default keep list.** jazari has no reliable way to tell whether Slack is
  in a huddle, so the founder decides with Slack's own button.
- **Calls in Chrome.** A Chrome tab can hold a Meet call. macOS has no public command that tells
  which app plays audio or uses the microphone. So jazari cannot detect such a call. For that
  reason, Close all leaves Chrome out, and Chrome closes only through its own button.
- **Chrome stays open while a session uses it.** If a session called a browser tool (any
  `mcp__claude-in-chrome__*` tool) in the last 10 minutes, Chrome is left out of the list.
- **Only app names go to Telegram.** Window titles can contain private text, so jazari never
  sends them.
- **`jazari watch pause` holds it**, like every other action that jazari does on its own.
- **Every request, answer and quit goes to the actions log** (`jazari watch actions`).

## Memory per app

An app's memory includes its helper processes. Chrome runs many "Google Chrome Helper"
renderers, and Electron apps such as Slack run helpers too. jazari groups processes by the `.app`
bundle that holds their executable (the outermost `*.app` folder in the path). Without this
grouping, the numbers would be far too low.

## Platforms

- macOS: full support, through `osascript -e 'tell application id "<bundle id>" to quit'`.
- Linux and Windows: no common polite Quit request exists. jazari lists the apps and their memory
  in the Telegram message and closes nothing. CI builds on all three systems, so this list-only
  path is the fallback.

## Settings

- `jazari watch focus --auto on|off`: ask on Telegram when memory reaches the warning level
  (off by default).
- `jazari watch focus --keep "<app>"` and `--unkeep "<app>"`: change the keep list.

## Parts and owners

1. **App list and quit (new `src/focus.rs`).** Lists the running GUI apps with their memory. It
   applies the keep list and the Chrome rule, and asks each app to quit. All system calls go
   through injectable functions, so tests never close a real app.
2. **The request file.** jazari writes `~/.jazari/claude/focus-request.json` with the app list,
   the one-time code and the expiry time. The bridge reads the answer from the button.
3. **The Telegram part (`src/jazari/bridge.rs`).** Only one program can read a bot's updates, so
   the request must go through the bridge. The bridge part stays small: it sends the card, edits
   it after a tap, and passes the tap to `src/focus.rs`. All the app logic stays in
   `src/focus.rs`. Each button's `callback_data` holds the one-time code and the app's number in
   the list (for example `fc:7f3a9c21:3`), never an app name, because Telegram allows at most
   64 bytes. Editing a card needs Telegram's `editMessageText`, which the bot module does not have
   yet. Owner: agent-context-enforce, who claims `bridge.rs` on the board before starting and
   tells the other agents that work in `src/jazari/`.
4. **The trigger.** The watcher (`src/watch.rs`) checks memory. At the warning level, with
   `--auto on`, it writes a request at most once every 60 minutes.

## Tests

- The keep list, the Chrome rule and the per-session rule, with a fake list of apps.
- The one-time code: an old code, a second tap and an expired request all close nothing.
- No answer in 10 minutes closes nothing.
- An app that refuses to quit is reported as "still open".
- The Telegram text holds app names only.

- A tap on one app's button closes that app only.
- Close all leaves Chrome and the frontmost app out.
- A Chrome Helper process counts toward Google Chrome's memory.
- Each `callback_data` is at most 64 bytes, with 50 apps in the list.
- With no bridge running, the request expires and closes nothing.
- A browser tool call with Chrome closed by jazari starts Chrome and refuses that one call with
  the retry text. With Chrome closed by the founder, jazari does not start it.
- `--reopen` opens only an app in `focus-closed.json`.

## Decisions

- 2026-11-26: the message has one button per app, as well as Close all and Not now.
- 2026-11-26: jazari opens a closed app again when a session needs it (the Chrome rule and
  `--reopen`). It does not open every closed app when the work ends.
- 2026-11-26 (review): the frontmost app and call apps are never closed. Close all leaves Chrome
  out. Memory counts helper processes. Only the approval card is pushed to Telegram.
