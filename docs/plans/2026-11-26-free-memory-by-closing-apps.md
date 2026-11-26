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
   > Google Chrome (1.4 GB), Slack (0.9 GB), Spotify (0.5 GB), Preview (0.3 GB)
   > [Close all] [Not now]

3. On **Close all**, jazari asks each app to quit and reports back: "Closed 3 apps. Preview is
   waiting for you to save a document, so it is still open."
4. On **Not now**, or if there is no answer in 10 minutes, jazari closes nothing. It asks again
   only after 60 minutes, and only if memory is still at the warning level.

At the desk, `jazari watch focus` shows the same list in the terminal and asks for a yes there.

## Rules that keep it safe

- **Only the paired chat can approve.** The bridge already drops messages from every other chat.
- **Each request has a one-time code.** The buttons carry a random code for that request. A tap
  on an old message, or a second tap, does nothing.
- **The approval covers only the apps in the message.** If the list changes before the tap,
  jazari closes only the apps that were on the list.
- **No answer means no close.**
- **Polite quit only.** On macOS, jazari sends each app the normal Quit request (as ⌘Q does). An
  app with unsaved work asks the founder to save and stays open. jazari never force-kills an app.
- **A keep list that jazari never closes:** terminals (Terminal, iTerm2, Warp, Ghostty, WezTerm,
  kitty), Finder, the Dock and other system processes, Claude, and every app that has a running
  Claude Code session as a child process. The founder can add apps to this list.
- **Chrome stays open while a session uses it.** If a session called a browser tool (any
  `mcp__claude-in-chrome__*` tool) in the last 10 minutes, Chrome is left out of the list.
- **Only app names go to Telegram.** Window titles can contain private text, so jazari never
  sends them.
- **`jazari watch pause` holds it**, like every other action that jazari does on its own.
- **Every request, answer and quit goes to the actions log** (`jazari watch actions`).

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
3. **The Telegram part (`src/jazari/bridge.rs`).** jazari-worker-2 owns this folder. The bridge
   sends the request with two buttons and records the tap. Only one program can read a bot's
   updates, so the request must go through the bridge.
4. **The trigger.** The watcher (`src/watch.rs`) checks memory. At the warning level, with
   `--auto on`, it writes a request at most once every 60 minutes.

## Tests

- The keep list, the Chrome rule and the per-session rule, with a fake list of apps.
- The one-time code: an old code, a second tap and an expired request all close nothing.
- No answer in 10 minutes closes nothing.
- An app that refuses to quit is reported as "still open".
- The Telegram text holds app names only.

## Open questions

- Should "Close all" also offer one button per app?
- Should jazari open the closed apps again when the work ends? This plan says no.
