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
tmuxor /path/to/repo --ui --task "Add retry logic to the HTTP client."
```

Requires Rust 1.89+ to build, Git, tmux, and the agent CLIs used by your team.
`--ui` opens a local browser dashboard. The installed binary includes all web
assets; Node and npm are only needed when developing the frontend.

Without `--ui`, tmuxor uses the terminal broker and opens terminal windows on
graphical Linux desktops. `--no-view` skips opening the browser or terminal
windows. In terminal mode, attach with `tmux attach -t tmuxor-broker`.

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
| `--perm <mode>` | Claude/Codex permission mode (default `auto`); ignored by OpenCode |
| `--yolo` | Claude/Codex `bypassPermissions`; ignored by OpenCode |
| `--clean` | tear down sessions, worktrees and branches first |
| `--ui` | use the browser dashboard and graphical broker |
| `--port PORT` | dashboard port; requires `--ui`, default `0` selects a free port |
| `--no-view` | print connection details without opening windows |

If an agent or broker process exits unexpectedly, its tmux pane remains open
with the exit status so you can inspect its output. Use `tmuxor stop <repo>`
when you want to close the sessions deliberately.

## Worktrees and code handoffs

Each role works in `.claude-worktrees/tmuxor-<role>` on its own `tmuxor/<role>`
branch. These are linked worktrees of one Git repository: they share commits
and branch refs, but have separate working files and indexes.

Agents stay in their own worktree for source inspection, edits, builds, and
tests. A builder commits the change and sends its branch, full commit SHA, and
checks run through the broker. The tester inspects and integrates that exact
commit into its own branch, then tests locally. For example, from the tester's
clean worktree:

```sh
git show <builder-commit-sha>
git diff HEAD <builder-commit-sha>
git merge --ff-only <builder-commit-sha>
# Run the project's tests here.
git rev-parse HEAD
```

No remote push, fetch, or pull is needed for these local handoffs. Agents use
the supplied SHA so a builder's later commits do not silently change the review.
The tester reports the received SHA and tested HEAD, and commits any new tests
before handing them back. Missing commits, dirty worktrees, or diverged branches
require coordination; agents must preserve existing work. A diverged branch may
need an agreed merge or specific cherry-picks rather than a fast-forward.

The generated instructions apply to Claude, Codex, and OpenCode and supersede
older role prompts that tell agents to read teammates' worktrees. The roster
lists teammates' branches, and Claude no longer receives `--add-dir` grants for
teammate directories. This workflow is guided by prompts, not enforced filesystem
isolation; native CLI permissions still apply.

After updating tmuxor, stop and relaunch the team to regenerate prompts and start
fresh agent sessions. Existing branches and worktrees are kept; `--clean` is not
needed. Custom files under `agora/roles/` are preserved. Update any old handoff
wording there or in skills to keep your project instructions consistent.

## Browser dashboard

```sh
tmuxor /path/to/repo --ui
# Choose a fixed port, without opening a browser automatically:
tmuxor /path/to/repo --ui --port 8787 --no-view
```

The dashboard has an agent sidebar, live interactive terminals, a message box
for giving an agent instructions, and a broker inbox. Focus on one agent or use
Split to watch the team. Native CLI permission prompts remain interactive inside
the embedded terminals. The message box adds the `[FROM OPERATOR]` header, so
Codex replies participate in routing just like other broker-delivered messages.

Select a pending reply to edit its text and recipient, then approve delivery or
drop it. Human/done messages are acknowledged without being sent to another
agent. Reviewed messages appear in Activity. If a delivery is interrupted, its
message remains visible for inspection and cannot be automatically resent.

Closing or refreshing the browser only detaches its terminal clients. Agent
sessions and the dashboard continue running in tmux. The dashboard URL is
printed at launch and saved in `.tmuxor/ui.json`. Use the full URL when opening
a new browser tab: its token grants access to the local terminals and broker.
The server listens on `127.0.0.1` only and bundles its assets without a CDN.

For an existing team whose broker has exited, start the dashboard in the
foreground with:

```sh
tmuxor ui /path/to/repo
```

Only one broker can own the queue. To switch from the terminal broker, press
`q` in that broker first, then run the command above. Ctrl-C stops this foreground
dashboard while keeping the agents running. `tmuxor stop <repo>` stops the tmux
team, including the dashboard started with `--ui`.

The browser launcher supports macOS (`open`) and Linux (`xdg-open`), and the
terminal bridge uses portable PTYs. Browser integration tests currently run on
Linux; macOS still needs a live verification run.

Activity indicators are approximate: input marks an agent active and a completion
message clears it. They cannot reliably identify every internal CLI state or
permission prompt; inspect the live terminal when in doubt.

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

Every adapter hands `relay-stop` the finished assistant text. OpenCode's plugin
reads structured messages through its SDK when the session becomes idle;
nothing scrapes terminal output or parses transcript files.

## Agent adapters

The Rust binary separates the shared runtime (worktrees, tmux panes, queue,
broker and delivery) from agent-specific setup. Everything an adapter varies
sits behind `Adapter::generate_files`; the broker, queue, routing and prompt
text never learn which CLI is on the other end.

| | `claude` | `codex` | `opencode` |
|---|---|---|---|
| end-of-turn callback | `Stop` hook | `notify` program | `session.idle` plugin |
| payload arrives as | JSON on stdin | JSON as `argv[1]`, piped to stdin by a shim | plugin pipes JSON to stdin |
| message field | `last_assistant_message` | `last-assistant-message` | normalized to `last_assistant_message` |
| per-agent config | `--settings FILE` | `--profile tmuxor-<role>` | `OPENCODE_CONFIG_DIR` |
| role prompt | `--append-system-prompt-file` | `developer_instructions` | custom primary agent selected with `--agent` |
| permissions | `--permission-mode` | `--ask-for-approval` + `--sandbox` | existing OpenCode settings |

`--perm` keeps Claude's vocabulary and maps onto codex: `auto`/`default` ->
`on-request` + `workspace-write`, `acceptEdits` -> `never` + `workspace-write`,
`plan` -> `untrusted` + `read-only`, `--yolo` ->
`--dangerously-bypass-approvals-and-sandbox`.

A new adapter needs the same three things: how to start the agent, where the
role prompt goes, and how a finished message reaches `tmuxor relay-stop`.

### OpenCode prompts and skills

Use `opencode` in `agora/team.txt`, on its own or alongside other adapters:

```text
claude specifier
opencode builder
opencode tester
```

Each OpenCode role gets its own primary agent named `tmuxor-<role>`, with the
shared rules, its role prompt, and routing instructions. It inherits OpenCode's
normal project and global settings, including models, providers, and permissions.
tmuxor does not map `--perm` or `--yolo` to OpenCode.

Add optional skills beside the role prompt:

```text
agora/roles/
├── builder.md
├── builder/
│   └── skills/
│       └── implementation/
│           ├── SKILL.md
│           └── references/
│               └── conventions.md
└── tester.md
```

`SKILL.md` uses [OpenCode's native skill format](https://opencode.ai/docs/skills/),
including `name` and `description` in YAML frontmatter. Only the builder's
OpenCode process discovers `builder/skills/`; project and global skills remain
available to every role. `--roles DIR` also moves this lookup to
`DIR/<role>/skills/`. These extra role skill folders currently apply to OpenCode.

tmuxor reserves `OPENCODE_CONFIG_DIR` for a generated directory under `.tmuxor/`.
It contains the agent config, the completion plugin, and a symlink to that role's
skills, so supporting files stay accessible. Project and global config files are
not edited. Restart the team after adding a new role skill directory.

The plugin relays only the latest completed assistant reply from a parent
session. It skips subagents, compaction summaries, failed or unfinished turns,
reasoning, and tool output, and suppresses duplicate idle notifications. Replies
to messages typed directly in an OpenCode pane also reach the broker. Relay
errors are recorded in `.tmuxor/relay.log`.

OpenCode runs the plugin itself; no separate Node installation or npm install is
needed for tmuxor's plugin. This integration targets the OpenCode 1.x plugin API
and has been checked with OpenCode 1.18.29.

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

**A worktree is not a filesystem sandbox.** It gives each role separate working
files and Git history for committed changes, but does not restrict what a shell
command can reach. Staying in the assigned worktree is a prompt instruction;
the agent CLI's permissions control actual access.

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

**Teammate code travels through Git.** Agents can inspect shared local commits
and integrate them into their own branch without access to teammates' working
files. Launchers do not add teammate directories with `--add-dir`.

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
src/ui.rs              local HTTP API, graphical broker and PTY/WebSocket bridge
src/opencode-plugin.mjs OpenCode completion plugin, embedded in the Rust binary
web/                   dashboard source and bundled xterm.js assets
Cargo.toml             Rust package and web/terminal dependencies
examples/team.txt     bundled example manifest
examples/roles/*.md   bundled example role prompts
tests/opencode-plugin.test.mjs  plugin/relay integration tests
web/tests/             browser tests with real tmux and offline fixture agents
```

Runtime state lives in `<repo>/.tmuxor/` (queue, generated prompts, settings,
launchers and notify shims, `panes.tsv`, `relay.log`). Worktrees go in
`<repo>/.claude-worktrees/`. The codex adapter also writes
`$CODEX_HOME/tmuxor-<role>.config.toml` (default `~/.codex`).
OpenCode's per-role config and plugin live in `.tmuxor/opencode-<role>/` and are
removed by `--clean` along with the other runtime files.
The dashboard also stores its roster in `.tmuxor/agents.json`, reviewed messages
in `.tmuxor/reviewed/`, and interrupted deliveries in `.tmuxor/inflight/`.

## Development checks

```sh
cargo test
cargo build
node tests/opencode-plugin.test.mjs
cargo clippy --all-targets -- -D warnings
```

The plugin tests use Node's built-in test runner and the built `tmuxor` binary to
check queue delivery without an AI provider or running tmux sessions.

To run the browser suite (Node, Python 3, Git, and tmux required):

```sh
cargo build
cd web
npm ci
npx playwright install chromium
npm test
```

The suite uses an isolated tmux socket and offline fixture agents. Set
`TMUXOR_CHROMIUM=/path/to/chrome` to use an existing Chromium installation.
After changing xterm.js versions, run `npm run vendor` and commit the updated
`web/vendor/` files. Other HTML, CSS, and JS edits are embedded by `cargo build`.
