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

Both adapters hand `relay-stop` a finished assistant message directly, so
nothing parses a transcript.

## Agent adapters

The Rust binary separates the shared runtime (worktrees, tmux panes, queue,
broker and delivery) from agent-specific setup. Everything an adapter varies
sits behind `Adapter::generate_files`; the broker, queue, routing and prompt
text never learn which CLI is on the other end.

| | `claude` | `codex` |
|---|---|---|
| end-of-turn callback | `Stop` hook in a generated settings file | `notify` program in a generated profile |
| payload arrives as | JSON on stdin | JSON as `argv[1]`, piped to stdin by a generated shim |
| message field | `last_assistant_message` | `last-assistant-message` |
| per-agent config | `--settings FILE` | `--profile tmuxor-<role>`, layered over your own config |
| role prompt | `--append-system-prompt-file` | `developer_instructions` |
| permissions | `--permission-mode` | `--ask-for-approval` + `--sandbox` |

`--perm` keeps Claude's vocabulary and maps onto codex: `auto`/`default` ->
`on-request` + `workspace-write`, `acceptEdits` -> `never` + `workspace-write`,
`plan` -> `untrusted` + `read-only`, `--yolo` ->
`--dangerously-bypass-approvals-and-sandbox`.

A third adapter needs the same three things: how to start the agent, where the
role prompt goes, and how a finished message reaches `tmuxor relay-stop`.

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
teammate. Codex `notify` fires for more than turn completion, so `relay-stop`
also drops any payload whose `type` is not `agent-turn-complete`.

**Codex profiles layer, so agents keep your real config.** A generated
`$CODEX_HOME/tmuxor-<role>.config.toml` adds only `notify`,
`developer_instructions` and trust entries; model, MCP servers and everything
else still come from your own `config.toml`. `--clean` deletes them.

**An untrusted directory blocks the first turn.** Codex prompts for trust in a
fresh worktree, and `--task` would paste into that prompt instead of the
composer, so the generated profile pre-trusts the repo and the agent's worktree.
That trust is scoped to the tmuxor profile, not written to your own config.

**Codex fires `notify` for its own internal turns.** Generating a thread title
is a real `agent-turn-complete` in its own thread, and its assistant message
(`{"title":"..."}`) was reaching the broker as an UNROUTED message; recap and
memory-consolidation turns would do the same. Blocklisting known internals would
rot as codex adds more, so the codex shim passes
`relay-stop --require-relayed-input` and only turns whose input *begins with*
tmuxor's `[FROM ...]` header are relayed. It must be `starts_with`, not
`contains`: the title prompt embeds the user's message verbatim under a
`User prompt:` heading, so a substring test passes on exactly the turn it is
meant to reject. The cost is that a message you type directly into a codex pane
is not auto-relayed — talk to a codex agent by hand and its reply stays in the
pane.

**Codex does not need `--add-dir` to read teammates.** There it grants *write*
access, which would break the one-writer-per-worktree rule; its sandbox already
allows reads outside the workspace.

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

Runtime state lives in `<repo>/.tmuxor/` (queue, generated prompts, settings,
launchers and notify shims, `panes.tsv`, `relay.log`). Worktrees go in
`<repo>/.claude-worktrees/`. The codex adapter also writes
`$CODEX_HOME/tmuxor-<role>.config.toml` (default `~/.codex`).
