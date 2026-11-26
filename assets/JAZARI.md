# Working with jazari

These rules apply to this conversation because it was started with them. The user's direct instructions in the conversation override them.

jazari is the dashboard that watches every Claude Code session on this machine. It keeps the machine from hanging (disk, memory, CPU), checks risky commands, and lets the user talk to and stop sessions while away from the desk. Treat it as the user's voice and the machine's state. If `jazari` is not on the PATH, the `jazari-usage` skill gives its full path.

## What jazari tells you

jazari's notes reach you as context beginning with `[jazari]`, at a tool call or a prompt:

- **A message from the user** ("The user sent this session a message through jazari"): act on it before going on. If it asks something and the user may not be watching this session, answer with `jazari watch say "..."` as well.
- **Disk almost full, or the machine busy**: finish the current step, start no builds, test runs, installs, image pulls or containers, and write a handoff (goal, what is done, decisions, next steps, file paths) so the work can go on later.
- **A refused tool call**: don't retry it or work around it. If it says to wait, run `jazari watch system --wait`, then try again. If it names a risky command, tell the user what you were trying to do.
- **Stopped from jazari**: stop at once and give a short summary of where the work stands.

## Telling jazari what you are doing

The user may follow this session only from jazari's dashboard, so keep it informed:

```sh
jazari watch say "migrating the orders table; tests next"              # shown on this session in the dashboard
jazari watch say --attention "need the staging API key to go on"      # also sounds and notifies the user
```

Post a line when you start a long step, finish a milestone, or get blocked. Use `--attention` only when you can't go on without the user, and say exactly what you need.

## Checking before heavy work

Before a build, a test run, a package install, a Docker build or pull, or a long unattended job:

```sh
jazari watch system --format json    # ok, concerns, disk, memory, swap, CPU
```

If `ok` is false, tell the user the `concerns` and prefer smaller steps. If memory or CPU is the problem, `jazari watch system --wait` returns once there is room (or after about two minutes; run it again if needed). If the disk is the problem, `jazari watch clean --plan` lists what can be freed. Run `jazari watch clean --tier-a` for rebuildable caches. Run anything else only with the user's agreement, and never delete such data yourself with `rm` or `docker`.

## Other sessions

`jazari watch status --format json` lists the other sessions on this machine: what each is doing and how much context it has left. Use it to avoid working on the same files at once, and to know when another session is busy with heavy work.
