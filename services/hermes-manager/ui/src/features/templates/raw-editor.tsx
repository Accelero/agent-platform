import { lookupReferences } from "../../components/reference";
import { useActorKey } from "../../app/state";
import { parseTree, findNodeAtOffset } from "jsonc-parser";
import { schemas } from "../../schemas/generated/metadata";
import { useEffect, useRef } from "react";
import { EditorState } from "@codemirror/state";
import { EditorView, keymap, lineNumbers } from "@codemirror/view";
import { defaultKeymap, history, historyKeymap } from "@codemirror/commands";
import { json, jsonParseLinter } from "@codemirror/lang-json";
import { autocompletion, completionKeymap } from "@codemirror/autocomplete";
import { linter, lintGutter } from "@codemirror/lint";
import { parseDraft, pointerRange, validateDraft } from "./model";
import type { ConfigurationProblem } from "../../api";
export default function RawEditor({
  value,
  onChange,
  problems = [],
  clientCompatible = true,
}: {
  value: string;
  onChange: (value: string) => void;
  problems?: ConfigurationProblem[];
  clientCompatible?: boolean;
}) {
  const actor = useActorKey();
  const actorRef = useRef(actor);
  actorRef.current = actor;
  const compatible = useRef(clientCompatible);
  compatible.current = clientCompatible;
  const parent = useRef<HTMLDivElement>(null);
  const editor = useRef<EditorView>(null);
  const changed = useRef(onChange);
  changed.current = onChange;
  const serverProblems = useRef(problems);
  serverProblems.current = problems;
  useEffect(() => {
    const view = new EditorView({
      parent: parent.current!,
      state: EditorState.create({
        doc: value,
        extensions: [
          lineNumbers(),
          history(),
          keymap.of([...completionKeymap, ...defaultKeymap, ...historyKeymap]),
          json(),
          lintGutter(),
          linter(jsonParseLinter()),
          linter((view) => {
            const text = view.state.doc.toString();
            const parsed = parseDraft(text);
            if (parsed.error)
              return [
                {
                  from: parsed.offset ?? 0,
                  to: Math.min((parsed.offset ?? 0) + 1, text.length),
                  severity: "error",
                  message: parsed.error,
                },
              ];
            return [
              ...(compatible.current ? validateDraft(parsed.value) : []),
              ...serverProblems.current,
            ].map((problem) => ({
              ...pointerRange(text, problem.pointer),
              severity: problem.severity,
              message: problem.message,
            }));
          }),
          EditorView.cspNonce.of(
            document.querySelector<HTMLMetaElement>(
              'meta[name="manager-style-nonce"]',
            )?.content ?? "",
          ),
          EditorView.contentAttributes.of({
            "aria-label": "Configuration JSON editor",
          }),
          EditorView.updateListener.of((update) => {
            if (update.docChanged) changed.current(update.state.doc.toString());
          }),
          autocompletion({
            interactionDelay: 0,
            override: [
              (context) => {
                if (!compatible.current) return null;
                const reference = context.matchBefore(
                  /"(?:storageClass|runtime_id)"\s*:\s*"[^"\n]*/,
                );
                if (reference) {
                  const opening = reference.text.lastIndexOf('"');
                  const kind = reference.text.startsWith('"storageClass"')
                    ? "storageClass"
                    : "runtime";
                  return lookupReferences(
                    actorRef.current,
                    kind,
                    reference.text.slice(opening + 1),
                  ).then((items) => ({
                    from: reference.from + opening,
                    to:
                      context.state.doc.sliceString(
                        context.pos,
                        context.pos + 1,
                      ) === '"'
                        ? context.pos + 1
                        : context.pos,
                    options: items.map((item) => ({
                      label: JSON.stringify(item.id),
                      info: item.label,
                      type: "value",
                    })),
                  }));
                }

                const word = context.matchBefore(/"?[a-zA-Z_]*/);
                if (!word || (!context.explicit && word.from === word.to))
                  return null;
                return {
                  from: word.from,
                  options: schemaOptions(
                    context.state.doc.toString(),
                    context.pos,
                  ),
                };
              },
            ],
          }),
        ],
      }),
    });
    editor.current = view;
    return () => {
      view.destroy();
      editor.current = null;
    };
  }, []);
  useEffect(() => {
    const view = editor.current;
    if (view && view.state.doc.toString() !== value)
      view.dispatch({
        changes: { from: 0, to: view.state.doc.length, insert: value },
      });
  }, [value]);
  return <div className="raw-editor" ref={parent} />;
}

type Schema = {
  properties?: Record<string, Schema>;
  description?: string;
  type?: unknown;
};
function schemaOptions(text: string, position: number) {
  const tree = parseTree(text);
  let node = tree ? findNodeAtOffset(tree, position, true) : undefined;
  while (node && node.type !== "object") node = node.parent;
  const path: string[] = [];
  for (let current = node; current?.parent; current = current.parent) {
    if (current.parent.type === "property")
      path.unshift(String(current.parent.children?.[0].value));
  }
  let schema = (path[0] === "adminConfig"
    ? schemas.managed
    : schemas.template) as unknown as Schema;
  for (const key of path[0] === "adminConfig" ? path.slice(1) : path)
    schema = schema.properties?.[key] ?? {};
  return Object.entries(schema.properties ?? {}).map(([name, property]) => ({
    label: JSON.stringify(name),
    type: "property",
    info:
      property.description ??
      `Configuration field (${String(property.type ?? "value")})`,
  }));
}
