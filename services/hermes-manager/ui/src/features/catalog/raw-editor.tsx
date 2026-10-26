import { parseTree, findNodeAtOffset } from "jsonc-parser";
import { schemas } from "../../schemas/generated/metadata";
import { useEffect, useRef } from "react";
import { EditorState } from "@codemirror/state";
import { EditorView, keymap, lineNumbers } from "@codemirror/view";
import { defaultKeymap, history, historyKeymap } from "@codemirror/commands";
import { json, jsonParseLinter } from "@codemirror/lang-json";
import { autocompletion, completionKeymap } from "@codemirror/autocomplete";
import { linter, lintGutter } from "@codemirror/lint";
import { validateDraft } from "./model";
import { yaml } from "@codemirror/lang-yaml";
import {
  parseConfiguration,
  configurationRange,
  type ConfigurationFormat,
} from "./yaml";
import type { CatalogCollection, ConfigurationProblem } from "../../api";
export default function RawEditor({
  collection,
  value,
  format = "json",
  onChange,
  problems = [],
  clientCompatible = true,
}: {
  collection: CatalogCollection;
  value: string;
  format?: ConfigurationFormat;
  onChange: (value: string) => void;
  problems?: ConfigurationProblem[];
  clientCompatible?: boolean;
}) {
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
          format === "json" ? json() : yaml(),
          lintGutter(),
          ...(format === "json" ? [linter(jsonParseLinter())] : []),
          linter((view) => {
            const text = view.state.doc.toString();
            const parsed = parseConfiguration(text, format);
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
              ...(compatible.current
                ? validateDraft(parsed.value, collection)
                : []),
              ...serverProblems.current,
            ].map((problem) => ({
              ...configurationRange(text, problem.pointer, format),
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
            "aria-label": `Configuration ${format.toUpperCase()} editor`,
          }),
          EditorView.updateListener.of((update) => {
            if (update.docChanged) changed.current(update.state.doc.toString());
          }),
          autocompletion({
            interactionDelay: 0,
            override: [
              (context) => {
                if (!compatible.current) return null;
                const word = context.matchBefore(/"?[a-zA-Z_]*/);
                if (!word || (!context.explicit && word.from === word.to))
                  return null;
                return {
                  from: word.from,
                  options:
                    format === "json"
                      ? schemaOptions(
                          collection,
                          context.state.doc.toString(),
                          context.pos,
                        )
                      : yamlOptions(
                          collection,
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
  }, [format, collection]);
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
function rootSchema(collection: CatalogCollection, inAdminConfig: boolean) {
  return (collection === "terminal-backends"
    ? schemas.backend
    : inAdminConfig
      ? schemas.managed
      : schemas.template) as unknown as Schema;
}
function schemaOptions(
  collection: CatalogCollection,
  text: string,
  position: number,
) {
  const tree = parseTree(text);
  let node = tree ? findNodeAtOffset(tree, position, true) : undefined;
  while (node && node.type !== "object") node = node.parent;
  const path: string[] = [];
  for (let current = node; current?.parent; current = current.parent) {
    if (current.parent.type === "property")
      path.unshift(String(current.parent.children?.[0].value));
  }
  const managed = collection === "templates" && path[0] === "adminConfig";
  let schema = rootSchema(collection, managed);
  for (const key of managed ? path.slice(1) : path)
    schema = schema.properties?.[key] ?? {};
  return Object.entries(schema.properties ?? {}).map(([name, property]) => ({
    label: JSON.stringify(name),
    type: "property",
    info:
      property.description ??
      `Configuration field (${String(property.type ?? "value")})`,
  }));
}

function yamlOptions(
  collection: CatalogCollection,
  text: string,
  position: number,
) {
  const lines = text.slice(0, position).split("\n");
  const currentIndent = lines.at(-1)!.match(/^ */)![0].length;
  const path: { key: string; indent: number }[] = [];
  for (const line of lines.slice(0, -1)) {
    const match = line.match(/^( *)([a-zA-Z_][a-zA-Z0-9_]*):/);
    if (!match) continue;
    while (path.length && path.at(-1)!.indent >= match[1].length) path.pop();
    path.push({ key: match[2], indent: match[1].length });
  }
  const keys = path.filter((p) => p.indent < currentIndent).map((p) => p.key);
  const managed = collection === "templates" && keys[0] === "adminConfig";
  let schema = rootSchema(collection, managed);
  for (const key of managed ? keys.slice(1) : keys)
    schema = schema.properties?.[key] ?? {};
  return Object.entries(schema.properties ?? {}).map(([name, property]) => ({
    label: name,
    apply: `${name}: `,
    type: "property",
    info:
      property.description ??
      `Configuration field (${String(property.type ?? "value")})`,
  }));
}
