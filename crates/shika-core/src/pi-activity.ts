// Loaded only by Shika's explicit --extension argument. No global installation.
import * as host from "@earendil-works/pi-coding-agent";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { Type } from "@earendil-works/pi-ai";
import { writeFileSync, renameSync, unlinkSync, openSync, readSync, closeSync, constants } from "node:fs";

export default function (pi: ExtensionAPI) {
  // Older Pi accepts unknown event names without reporting an error. Do not
  // publish a misleading Working forever on hosts without agent_settled.
  // That event was introduced in 0.80.4. Unknown versions stay on fallback.
  const version = /^(\d+)\.(\d+)\.(\d+)$/.exec(host.VERSION ?? "");
  if (!version) return;
  const [major, minor, patch] = version.slice(1).map(Number);
  if (major === 0 && (minor < 80 || (minor === 80 && patch < 4))) return;

  // The model names its own session during the normal turn, not through an
  // extra model request. Never overwrite a name from /name or another extension.
  pi.registerTool({
    name: "shika_name_session",
    label: "Name session",
    description: "Give this unnamed Pi conversation a short, descriptive task title. Call early, before investigating or changing files. Does not edit files or run Git.",
    promptGuidelines: [
      "At the start of a task, call shika_name_session with a concise title (3 to 8 words) describing the user's goal, before other tools. Summarize the request, not an attachment path. If the goal is unclear, ask first and name it once understood. Existing session names are preserved; do not call again on later turns.",
    ],
    parameters: Type.Object({
      name: Type.String({ description: "Short task title, one line, at most 80 characters", minLength: 1, maxLength: 80 }),
    }),
    async execute(_id, params) {
      const current = pi.getSessionName();
      if (current?.trim()) {
        return { content: [{ type: "text", text: `Session already named: ${current}` }], details: undefined };
      }
      const name = params.name.trim();
      if (!name || /[\r\n\u2028\u2029]/u.test(name) || [...name].length > 80) {
        throw new Error("Use a nonempty, single-line title of at most 80 characters.");
      }
      pi.setSessionName(name);
      return { content: [{ type: "text", text: `Session named: ${name}` }], details: undefined };
    },
  });

  let file: string | undefined;
  let seq = 0;
  function report(state: "idle" | "working") {
    if (!file || seq >= Number.MAX_SAFE_INTEGER) return;
    const temporary = `${file}.next`;
    try {
      // Synchronous tiny writes serialize event order; rename exposes either
      // complete old or complete new metadata, never a partial JSON document.
      writeFileSync(temporary, JSON.stringify({ seq: ++seq, state }), { mode: 0o600 });
      renameSync(temporary, file);
    } catch {
      // Metadata must never disrupt the CLI, including after Shika cleanup.
      try { unlinkSync(temporary); } catch {}
    }
  }

  pi.on("session_start", () => {
    file = process.env.SHIKA_ACTIVITY_FILE;
    if (!file) return;
    // /reload replaces the extension instance. Continue the same sequence
    // rather than making the reader accept a stale generation.
    let descriptor: number | undefined;
    try {
      descriptor = openSync(file, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
      const bytes = Buffer.alloc(129);
      const length = readSync(descriptor, bytes, 0, bytes.length, 0);
      if (length <= 128) {
        const previous = JSON.parse(bytes.subarray(0, length).toString("utf8"));
        if (Number.isSafeInteger(previous.seq) && previous.seq > seq) seq = previous.seq;
      }
    } catch {} finally {
      if (descriptor !== undefined) { try { closeSync(descriptor); } catch {} }
    }
    report("idle");
  });
  pi.on("agent_start", () => report("working"));
  // agent_end is not final: automatic continuations and retries can follow.
  pi.on("agent_settled", () => report("idle"));
  pi.on("session_shutdown", () => {
    report("idle");
    file = undefined;
  });
}
