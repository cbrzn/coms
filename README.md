# tmuxor

A human-gated router for terminal coding agents. Each agent runs in its own tmux
session and git worktree, with **you as the gate between them**. Agents decide
who to talk to; nothing is relayed until you approve it.

```
tmuxor-specifier ─┐
tmuxor-builder   ─┼─→ tmuxor-broker  →  [s]end [r]etarget [e]dit [d]rop
tmuxor-tester    ─┘        (you)
```

## Use

```sh
cargo install --path .
tmuxor /path/to/repo --task "Add retry logic to the HTTP client."
```

On a graphical Linux desktop, tmuxor opens a terminal for the broker and each
agent session automatically. Use `--no-view` for a headless run, then attach
manually with `tmux attach -t tmuxor-broker`.

Stop a running team without deleting its worktrees or runtime files:

```sh
tmuxor stop /path/to/repo
```

Define the team in `agora/team.txt` — one agent per line:

```
claude specifier
claude builder
claude tester
```

Use this project layout:

```
your-repo/
└── agora/
    ├── team.txt
    ├── rules/
    │   ├── 00-project.md
    │   └── 10-testing.md
    └── roles/
        ├── specifier.md
        ├── builder.md
        └── tester.md
```

The first agent is the entry point unless another is marked `--entry`. The first
field is the agent adapter and the second is its role. Every agent receives the
Markdown files in `agora/rules/`, in filename order, followed by its unique
role prompt from `agora/roles/<name>.md` and the generated roster/routing
instructions.

`agora/` takes precedence over root-level `manifest`, `team.txt`, `roles/`, and
`rules/`. Override any location explicitly with `--team`, `--roles`, or
`--rules`.

| flag | meaning |
| --- | --- |
| `--task "..."` | opening instruction, pasted into the entry agent |
| `--team FILE` | team manifest (default: `agora/team.txt`) |
| `--roles DIR` | role prompt directory (default: `agora/roles/`) |
| `--rules DIR` | shared Markdown rules directory (default: `agora/rules/`) |
| `--perm <mode>` | permission mode (default `auto`) |
| `--yolo` | `bypassPermissions` — never prompts |
| `--clean` | tear down sessions, worktrees and branches first |
| `--no-view` | do not open terminal windows automatically |

If an agent or broker process exits unexpectedly, its tmux pane remains open
with the exit status so you can inspect its output. Use `tmuxor stop <repo>`
when you want to close the sessions deliberately.

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

1. An agent finishes its turn; its completion hook fires.
2. `tmuxor relay-stop` parses the routing line and writes `{from, to, text}`
   into `.tmuxor/queue/`, then exits 0 — non-blocking, so the agent goes idle.
3. The broker shows you the message and who it is addressed to, and waits.
4. On approval it tags the message `[FROM SPECIFIER]`, pastes it into the
   recipient's pane, and submits.

For the Claude adapter, the Stop payload includes `last_assistant_message`
directly, so nothing parses the transcript JSONL.

## Agent adapters

The Rust binary separates the shared runtime (worktrees, tmux panes, queue,
broker and delivery) from agent-specific setup. `claude` is the currently
implemented adapter: it generates Claude settings, injects its Stop hook, and
starts Claude with the role prompt.

This makes other agents a contained addition rather than a rewrite of the
broker. An adapter needs to define how to start the agent, supply the generated
role prompt, and surface a completed assistant message to `tmuxor relay-stop`
(or an equivalent event source). Codex and OpenCode can use the same routing and
broker once their adapter hooks or completion-monitoring mechanism are defined.

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

**Subagent turns fire `Stop` too.** The Claude adapter ignores any payload
carrying an `agent_id`; a subagent's report is internal, not a message to a
teammate.

**Identity travels as env, not config.** All worktrees share the repo, so a
committed agent settings file would apply to every agent. Each role gets a
generated adapter settings file plus `TMUXOR_ROLE`/`TMUXOR_HOME` in its
environment.

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
src/main.rs            launcher, broker and completion-hook subcommands
Cargo.toml             dependency-free Rust package
examples/team.txt     bundled example manifest
examples/roles/*.md   bundled example role prompts
```

Runtime state lives in `<repo>/.tmuxor/` (queue, generated prompts and settings,
`panes.tsv`, `relay.log`). Worktrees go in `<repo>/.claude-worktrees/`.
