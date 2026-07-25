# duo

Two Claude Code instances pairing on one repo — a builder and a critic — each in
its own git worktree, with **you as the gate between them**. Nothing either one
says reaches the other until you approve it.

```
┌──────────────────┬──────────────────┐
│ BUILDER          │ CRITIC           │   each: claude, own worktree,
│ .../duo-builder  │ .../duo-critic   │   read access to the other's
├──────────────────┴──────────────────┤
│ broker — [s]end [e]dit [d]rop       │
└─────────────────────────────────────┘
```

## Use

```sh
bin/duo /path/to/repo --task "Add retry logic to the HTTP client."
tmux attach -t duo
```

| flag | meaning |
| --- | --- |
| `--task "..."` | opening instruction, pasted into builder at startup |
| `--perm <mode>` | permission mode (default `auto`) |
| `--yolo` | `bypassPermissions` — never prompts |
| `--clean` | tear down session, worktrees, and branches first |

In the broker pane: `s` send, `e` edit in `$EDITOR` then re-review, `d` drop,
`q` quit. Editing is the point — trim the message, correct a wrong premise, or
add a steer, then send.

## How it works

1. An instance finishes its turn. Its `Stop` hook fires.
2. `hooks/relay-stop.sh` writes `{from, text}` into `.duo/queue/` and exits 0.
   Non-blocking — the instance just goes idle.
3. The broker shows you the message and waits.
4. On approval it tags the message `[FROM BUILDER]`, delivers it to the peer's
   pane, and submits.

The Stop hook payload includes `last_assistant_message` directly, so nothing
parses the transcript JSONL.

## Things that were not obvious

**Delivery must be a bracketed paste, not `send-keys`.** `send-keys` with a
multi-line string submits at the first newline, so the peer receives only line
one. `tmux load-buffer` + `paste-buffer -p` delivers the whole message with no
trailing newline; a separate `send-keys Enter` submits it. Verified against a
live TUI — all lines land in the composer, nothing submits early.

**`acceptEdits` is not enough.** It auto-accepts file edits but Bash still
prompts, so a pane stalls on a permission dialog and you are back to babysitting
three panes. Default is `auto`, whose classifier approves routine work and stops
the risky calls.

**A worktree bounds writes, not blast radius.** It keeps the two agents off each
other's files and gives you a git-level undo. It does not sandbox what a shell
command can reach. `--yolo` removes the last check — know that before using it.

**Subagent turns fire `Stop` too.** The hook ignores any payload carrying an
`agent_id`; a subagent's report is internal, not a message to the peer.

**Identity travels as env, not config.** Both worktrees share the repo, so a
committed `.claude/settings.json` would apply to both. Each role gets its own
generated settings file plus `DUO_ROLE`/`DUO_HOME` in the pane environment.

**Panes are addressed by id (`%9`), not index.** Pane indices renumber when a
pane dies; ids never do.

**tmux runs pane commands through `sh`.** On Ubuntu that is dash, which does not
understand the `$'...'` quoting bash emits for multi-line strings. Each role
gets a generated launcher script so the pane command is a bare path.

## Layout

```
bin/duo               launcher — worktrees, panes, settings, prompts
bin/duo-broker        the review gate
hooks/relay-stop.sh   Stop hook — queues a message, never sends it
```

Runtime state lives in `<repo>/.duo/` (queue, per-role prompts and settings,
`panes.env`, `relay.log`). Worktrees go in `<repo>/.claude-worktrees/`.
