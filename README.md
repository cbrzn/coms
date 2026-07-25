# tmuxor

A team of Claude Code agents that talk to each other, each in its own tmux
session and its own git worktree, with **you as the gate between them**. Agents
decide who to talk to; nothing is relayed until you approve it.

```
tmuxor-specifier ─┐
tmuxor-builder   ─┼─→ tmuxor-broker  →  [s]end [r]etarget [e]dit [d]rop
tmuxor-tester    ─┘        (you)
```

## Use

```sh
bin/tmuxor /path/to/repo --task "Add retry logic to the HTTP client."
tmux attach -t tmuxor-broker
```

Define the team in `team.txt` — one agent per line:

```
claude specifier
claude builder
claude tester
```

The first agent is the entry point unless another is marked `--entry`. Each
needs a prompt at `roles/<name>.md` — plain prose, no boilerplate. The launcher
appends a generated roster so every agent knows who its teammates are, what they
do, and where their worktrees are.

A repo may carry its own `team.txt` and `roles/`; they take precedence over the
ones here. Override explicitly with `--team` / `--roles`.

| flag | meaning |
| --- | --- |
| `--task "..."` | opening instruction, pasted into the entry agent |
| `--team FILE` | team manifest (default: repo's, else this one) |
| `--roles DIR` | role prompt directory |
| `--perm <mode>` | permission mode (default `auto`) |
| `--yolo` | `bypassPermissions` — never prompts |
| `--clean` | tear down sessions, worktrees and branches first |

## Routing

An agent addresses a teammate with a routing line of its own:

```
[TO: builder]
```

`[TO: human]` asks for you; `[TO: done]` ends the chain. Both park at the broker
rather than being relayed. If a message carries no routing line — or more than
one — the broker asks you to pick rather than guessing.

In the broker: `s` send, `r` retarget, `e` edit in `$EDITOR`, `d` drop, `q` quit.

## How it works

1. An agent finishes its turn; its `Stop` hook fires.
2. `hooks/relay-stop.sh` parses the routing line and writes `{from, to, text}`
   into `.tmuxor/queue/`, then exits 0 — non-blocking, so the agent goes idle.
3. The broker shows you the message and who it is addressed to, and waits.
4. On approval it tags the message `[FROM SPECIFIER]`, pastes it into the
   recipient's pane, and submits.

The Stop payload includes `last_assistant_message` directly, so nothing parses
the transcript JSONL.

## Things that were not obvious

**Delivery must be a bracketed paste, not `send-keys`.** `send-keys` with a
multi-line string submits at the first newline, so the recipient gets line one
only. `tmux load-buffer` + `paste-buffer -p` delivers the whole message with no
trailing newline; a separate `send-keys Enter` submits it.

**Pane ids are global to the tmux server.** `%17` is addressable from any
session, which is the only reason one broker can drive agents that each live in
a separate session.

**Requiring the routing tag on line one does not survive a real model.** In
testing, an agent opened with a sentence of preamble and put `[TO: builder]`
after it. The tag is now accepted on any line of its own, provided there is
exactly one — zero or several, and the broker asks you.

**`acceptEdits` is not enough.** It auto-accepts file edits but Bash still
prompts, so an agent stalls mid-turn on a permission dialog and you are back to
babysitting every session. Default is `auto`, whose classifier approves routine
work and stops the risky calls.

**A worktree bounds writes, not blast radius.** It keeps agents off each other's
files and gives you a git-level undo. It does not sandbox what a shell command
can reach. `--yolo` removes the last check.

**Subagent turns fire `Stop` too.** The hook ignores any payload carrying an
`agent_id`; a subagent's report is internal, not a message to a teammate.

**Identity travels as env, not config.** All worktrees share the repo, so a
committed `.claude/settings.json` would apply to every agent. Each role gets a
generated settings file plus `TMUXOR_ROLE`/`TMUXOR_HOME` in its environment.

**tmux runs pane commands through `sh`.** On Ubuntu that is dash, which does not
understand the `$'...'` quoting bash emits for multi-line strings. Each role gets
a generated launcher script so the pane command is a bare path.

## Known rough edges

- A message can arrive while its recipient is mid-turn. The broker tracks who is
  busy and warns before delivering, but does not hold the message back.
- One recipient per message; no fan-out.
- Role prompts need iterating. Whether an agent routes sensibly is a prompt
  problem, not a transport problem.

## Layout

```
bin/tmuxor            launcher — team file, worktrees, sessions, prompts
bin/tmuxor-broker     the review gate
hooks/relay-stop.sh   Stop hook — parses routing, queues, never sends
team.txt              example manifest
roles/*.md            example role prompts
```

Runtime state lives in `<repo>/.tmuxor/` (queue, generated prompts and settings,
`panes.tsv`, `relay.log`). Worktrees go in `<repo>/.claude-worktrees/`.
