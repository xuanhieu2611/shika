// Loaded only by Shika's explicit --extension argument, for a chat Lead in
// Pi's RPC mode. No global installation.
//
// Stock Pi has no question tool. This one opens a single extension `input`
// dialog titled `shika-question:<tool call id>`. Shika reads the questions
// from this tool's own `tool_execution_start` arguments, shows its native
// question dialog, and answers the input with a JSON object that maps each
// question's text to the chosen label, the labels (several choices), or the
// author's own words. A cancelled dialog resolves to undefined.
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { Type } from "@earendil-works/pi-ai";

const Option = Type.Object({
  label: Type.String({ description: "A short choice, 1 to 5 words", minLength: 1, maxLength: 80 }),
  description: Type.Optional(Type.String({ description: "What choosing it means", maxLength: 300 })),
});

const Question = Type.Object({
  question: Type.String({ description: "The complete question, ending with a question mark", minLength: 1, maxLength: 1000 }),
  header: Type.Optional(Type.String({ description: "A very short label for the question", maxLength: 30 })),
  options: Type.Optional(Type.Array(Option, { description: "Up to 4 choices. Omit for a free-form answer.", maxItems: 4 })),
  multiSelect: Type.Optional(Type.Boolean({ description: "Allow choosing more than one option" })),
});

function reply(text: string) {
  return { content: [{ type: "text" as const, text }], details: undefined };
}

export default function (pi: ExtensionAPI) {
  pi.registerTool({
    name: "shika_ask_author",
    label: "Ask the author",
    description:
      "Ask the author 1 to 4 questions in a dialog and wait for the answers. Each question can offer up to 4 options; the author may also write their own answer or cancel. Use it when you need the author's decision to continue.",
    parameters: Type.Object({
      questions: Type.Array(Question, { minItems: 1, maxItems: 4 }),
    }),
    executionMode: "sequential",
    async execute(toolCallId, params, signal, _onUpdate, ctx) {
      if (ctx.mode !== "rpc") {
        return reply("No author dialog is available here. Ask in your reply instead.");
      }
      const texts = params.questions.map((q) => q.question.trim());
      if (texts.some((text) => !text) || new Set(texts).size !== texts.length) {
        throw new Error("Each question needs its own nonempty text.");
      }
      // The abort signal dismisses the dialog when the author stops the reply.
      const value = await ctx.ui.input(`shika-question:${toolCallId}`, undefined, { signal });
      if (value === undefined) {
        return reply("The author cancelled the question without answering.");
      }
      let answers: Record<string, unknown> = {};
      try {
        const parsed = JSON.parse(value);
        if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) answers = parsed;
      } catch {
        return reply(`The author answered: ${value}`);
      }
      const lines = params.questions.map((q) => {
        const given = answers[q.question];
        const answer = Array.isArray(given)
          ? given.filter((a) => typeof a === "string").join(", ")
          : typeof given === "string"
            ? given
            : "";
        return `${q.question}\nAnswer: ${answer || "(no answer)"}`;
      });
      return reply(lines.join("\n\n"));
    },
  });
}
