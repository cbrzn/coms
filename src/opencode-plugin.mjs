import { spawn } from "node:child_process";
import { appendFile } from "node:fs/promises";
import { join } from "node:path";

// Copied into each role's generated OpenCode config directory. No npm packages
// are needed: the host supplies the SDK client and runs this plugin itself.
export const TmuxorPlugin = async ({ client, directory }) => {
  const home = process.env.TMUXOR_HOME;
  const role = process.env.TMUXOR_ROLE;
  const executable = process.env.TMUXOR_EXECUTABLE;
  if (!home || !role || !executable) return {};

  const delivered = new Map();
  const pending = new Map();

  async function relay(sessionID) {
    const options = {
      path: { id: sessionID },
      query: { directory },
      throwOnError: true,
    };
    const { data: session } = await client.session.get(options);
    // Task subagents have their own sessions; their reports belong to the
    // parent agent, never directly to a tmuxor teammate.
    if (!session || session.parentID) return;

    const { data: messages } = await client.session.messages(options);
    const userIndex = messages.findLastIndex((m) => m.info.role === "user");
    const answerIndex = messages.findLastIndex((m) => m.info.role === "assistant");
    if (userIndex < 0 || answerIndex <= userIndex) return;
    const { info, parts } = messages[answerIndex];
    if (
      info.parentID !== messages[userIndex].info.id ||
      !info.time.completed ||
      info.error ||
      info.summary ||
      !info.finish ||
      info.finish === "tool-calls" ||
      info.finish === "unknown" ||
      delivered.get(sessionID) === info.id
    ) return;

    const text = parts
      .filter((part) => part.type === "text" && !part.synthetic && !part.ignored)
      .map((part) => part.text)
      .join("\n\n");
    if (!text.trim()) return;

    await new Promise((resolve, reject) => {
      const child = spawn(executable, ["relay-stop"], {
        env: { ...process.env, TMUXOR_HOME: home, TMUXOR_ROLE: role },
        stdio: ["pipe", "ignore", "pipe"],
      });
      let stderr = "";
      child.stderr.on("data", (chunk) => { stderr += chunk; });
      child.on("error", reject);
      child.stdin.on("error", reject);
      child.on("close", (code) => {
        if (code === 0) resolve();
        else reject(new Error(`relay-stop exited ${code}: ${stderr.trim()}`));
      });
      child.stdin.end(JSON.stringify({ last_assistant_message: text }));
    });
    delivered.set(sessionID, info.id);
  }

  return {
    event: async ({ event }) => {
      if (event.type !== "session.idle") return;
      const sessionID = event.properties.sessionID;
      // OpenCode can emit overlapping idle events. Serialize them so a final
      // reply is queued only once, even while the relay process is starting.
      const task = (pending.get(sessionID) ?? Promise.resolve())
        .then(() => relay(sessionID))
        .catch(async (error) => {
          const message = `${new Date().toISOString()} opencode ${role}: ${error}\n`;
          await appendFile(join(home, "relay.log"), message).catch(() => {});
        });
      pending.set(sessionID, task);
      await task;
      if (pending.get(sessionID) === task) pending.delete(sessionID);
    },
  };
};
