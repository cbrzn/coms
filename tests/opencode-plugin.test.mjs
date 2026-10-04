import assert from "node:assert/strict";
import { chmod, mkdtemp, mkdir, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { TmuxorPlugin } from "../src/opencode-plugin.mjs";

const executable = fileURLToPath(new URL("../target/debug/tmuxor", import.meta.url));
const user = (id = "user-1") => ({
  info: { id, role: "user" },
  parts: [{ type: "text", text: "[FROM OPERATOR]\n\nBuild it." }],
});
const answer = (info = {}, parts = [{ type: "text", text: "[TO: tester]\nReady." }]) => ({
  info: {
    id: "answer-1", role: "assistant", parentID: "user-1",
    time: { completed: 123 }, finish: "stop", ...info,
  },
  parts,
});

async function fixture(t, options = {}) {
  const home = await mkdtemp(join(tmpdir(), "tmuxor-plugin-"));
  await mkdir(join(home, "queue"));
  t.after(() => rm(home, { recursive: true, force: true }));
  const env = {
    TMUXOR_HOME: home,
    TMUXOR_ROLE: "builder",
    TMUXOR_EXECUTABLE: options.executable ?? executable,
  };
  const saved = Object.fromEntries(Object.keys(env).map((key) => [key, process.env[key]]));
  Object.assign(process.env, env);
  const state = {
    session: { id: "session-1" },
    messages: [user(), answer()],
    failure: undefined,
    reads: 0,
  };
  const directory = join(home, "worktree");
  const client = {
    session: {
      get: async (options) => {
        assert.equal(options.path.id, "session-1");
        assert.equal(options.query.directory, directory);
        assert.equal(options.throwOnError, true);
        if (state.failure) throw state.failure;
        return { data: state.session };
      },
      messages: async (options) => {
        assert.equal(options.query.directory, directory);
        state.reads++;
        return { data: state.messages };
      },
    },
  };
  let plugin;
  try {
    plugin = await TmuxorPlugin({ client, directory });
  } finally {
    for (const [key, value] of Object.entries(saved)) {
      if (value === undefined) delete process.env[key];
      else process.env[key] = value;
    }
  }
  return {
    home, state, plugin,
    idle: () => plugin.event({ event: { type: "session.idle", properties: { sessionID: "session-1" } } }),
    queue: async () => Promise.all((await readdir(join(home, "queue"))).sort()
      .map(async (file) => JSON.parse(await readFile(join(home, "queue", file), "utf8")))),
  };
}

test("only the final visible reply reaches the real relay-stop queue", async (t) => {
  const f = await fixture(t);
  f.state.messages = [
    user(),
    answer({ id: "tool-step", finish: "tool-calls" }),
    answer({}, [
      { type: "reasoning", text: "Private reasoning" },
      { type: "tool", text: "Tool output" },
      { type: "text", text: "[TO: tester]\nReady: \"quoted\" — café." },
      { type: "text", text: "Synthetic note", synthetic: true },
      { type: "text", text: "Ignored note", ignored: true },
      { type: "text", text: "Check the second line.\nAnd this one." },
    ]),
  ];
  await f.idle();
  assert.deepEqual(await f.queue(), [{
    from: "builder", to: "tester",
    text: "Ready: \"quoted\" — café.\n\nCheck the second line.\nAnd this one.",
  }]);
});

test("duplicate and overlapping idle events queue one reply per turn", async (t) => {
  const f = await fixture(t);
  await Promise.all([f.idle(), f.idle(), f.idle()]);
  await f.idle();
  assert.equal((await f.queue()).length, 1);
  f.state.messages.push(user("user-2"), answer({ id: "answer-2", parentID: "user-2" }));
  await f.idle();
  assert.equal((await f.queue()).length, 2);
});

test("subagent sessions and unrelated events never relay", async (t) => {
  const f = await fixture(t);
  await f.plugin.event({ event: { type: "message.updated", properties: {} } });
  assert.equal(f.state.reads, 0);
  f.state.session.parentID = "parent-session";
  await f.idle();
  assert.equal(f.state.reads, 0);
  assert.deepEqual(await f.queue(), []);
});

test("idle during failed, unfinished, internal, or empty turns does not replay old answers", async (t) => {
  const f = await fixture(t);
  const cases = [
    [],
    [user()],
    [answer()],
    [user(), answer(), user("user-2")],
    [user(), answer({ time: {} })],
    [user(), answer({ error: { name: "MessageAbortedError" } })],
    [user(), answer({ summary: true })],
    [user(), answer({ finish: "tool-calls" })],
    [user(), answer({ finish: "unknown" })],
    [user(), answer({ finish: undefined })],
    [user(), answer({ parentID: "another-user" })],
    [user(), answer({}, [{ type: "reasoning", text: "Thinking" }])],
    [user(), answer({}, [{ type: "text", text: " " }])],
    [user(), answer(), answer({ id: "failed-latest", error: {} })],
  ];
  for (const messages of cases) {
    f.state.messages = messages;
    await f.idle();
    assert.deepEqual(await f.queue(), [], JSON.stringify(messages));
  }
});

test("direct user turns and unrouted answers still reach the human broker", async (t) => {
  const f = await fixture(t);
  f.state.messages = [user(), answer({}, [{ type: "text", text: "Please clarify." }])];
  f.state.messages[0].parts[0].text = "A manually typed message";
  await f.idle();
  assert.deepEqual(await f.queue(), [{ from: "builder", to: "", text: "Please clarify." }]);
});

test("SDK failures are logged and the next event can retry", async (t) => {
  const f = await fixture(t);
  f.state.failure = new Error("SDK unavailable");
  await f.idle();
  assert.match(await readFile(join(f.home, "relay.log"), "utf8"), /SDK unavailable/);
  assert.deepEqual(await f.queue(), []);
  f.state.failure = undefined;
  await f.idle();
  assert.equal((await f.queue()).length, 1);
});

test("relay process failure is logged without marking the answer delivered", async (t) => {
  const dir = await mkdtemp(join(tmpdir(), "tmuxor-relay-"));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const wrapper = join(dir, "relay");
  await writeFile(wrapper, "#!/bin/sh\ncat >/dev/null\nexit 1\n");
  await chmod(wrapper, 0o755);
  const f = await fixture(t, { executable: wrapper });
  await f.idle();
  assert.match(await readFile(join(f.home, "relay.log"), "utf8"), /relay-stop exited 1/);
  assert.deepEqual(await f.queue(), []);
  await writeFile(wrapper, `#!/bin/sh\nexec '${executable.replaceAll("'", "'\"'\"'")}' "$@"\n`);
  await f.idle();
  assert.equal((await f.queue()).length, 1);
});

test("plugin is inert outside tmuxor", async () => {
  const saved = process.env.TMUXOR_HOME;
  delete process.env.TMUXOR_HOME;
  try {
    assert.deepEqual(await TmuxorPlugin({}), {});
  } finally {
    if (saved !== undefined) process.env.TMUXOR_HOME = saved;
  }
});
