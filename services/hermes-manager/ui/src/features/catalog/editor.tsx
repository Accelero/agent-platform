import { useEffect, useState } from "react";
import { useMutation } from "@tanstack/react-query";
import {
  ApiError,
  api,
  createIntent,
  displayProblems,
  type CatalogCollection,
  type ConfigurationProblem,
  type Result,
} from "../../api";
import { queryClient, registerPrivateReset, useSession } from "../../app/state";
import { ProblemList } from "../../components/command";
import { ErrorNotice } from "../../components/data";
import { validateDraft, type Draft } from "./model";
import RawEditor from "./raw-editor";
import {
  parseConfiguration,
  serializeConfiguration,
  type ConfigurationFormat,
} from "./yaml";

type Receipt = Result<"createTemplateDraft">;
/**
 * Edits one draft as JSON or YAML. Both are views of one canonical JSON
 * document; invalid text blocks switching formats and saving.
 */
export default function DraftEditor({
  collection,
  id,
  version,
  etag,
  base,
  onSaved,
}: {
  collection: CatalogCollection;
  id: string;
  /** The draft being edited; absent when this creates the next version. */
  version?: number;
  etag?: string | null;
  base: Draft;
  onSaved: (receipt: Receipt) => void;
}) {
  const session = useSession();
  const [format, setFormat] = useState<ConfigurationFormat>("yaml");
  const [text, setText] = useState(() => serializeConfiguration(base, "yaml"));
  const [server, setServer] = useState<ConfigurationProblem[]>([]);
  const [submit] = useState(() => createIntent());
  useEffect(
    () =>
      registerPrivateReset(() => setText(serializeConfiguration({}, "yaml"))),
    [],
  );
  const parsed = parseConfiguration(text, format);
  const local = parsed.value ? validateDraft(parsed.value, collection) : [];
  const errors = local.filter((problem) => problem.severity === "error");
  const validate = useMutation({
    mutationFn: () =>
      api.validate(collection, parsed.value!, session.data!.csrfToken!),
    onSuccess: (result) => setServer(displayProblems(result)),
  });
  const save = useMutation({
    mutationFn: () =>
      version === undefined
        ? submit<Receipt>(
            "POST",
            `/api/v1/${collection}/${encodeURIComponent(id)}/versions`,
            { definition: parsed.value },
            session.data!.csrfToken!,
          )
        : submit<Receipt>(
            "PATCH",
            `/api/v1/${collection}/${encodeURIComponent(id)}/versions/${version}`,
            { definition: parsed.value },
            session.data!.csrfToken!,
            etag,
          ),
    onSuccess: (receipt) => {
      void queryClient.invalidateQueries();
      onSaved(receipt);
    },
    onError: (error) => {
      if (error instanceof ApiError) setServer(error.problems);
    },
  });
  return (
    <section className="panel detail" aria-label="Draft editor">
      <h2>
        {version === undefined
          ? `New version of ${id}`
          : `Draft ${id} v${version}`}
      </h2>
      <div className="tabs" role="tablist" aria-label="Format">
        {(["yaml", "json"] as const).map((value) => (
          <button
            key={value}
            role="tab"
            aria-selected={format === value}
            disabled={!parsed.value && format !== value}
            onClick={() => {
              if (!parsed.value || format === value) return;
              setText(serializeConfiguration(parsed.value, value));
              setFormat(value);
            }}
          >
            {value.toUpperCase()}
          </button>
        ))}
      </div>
      <RawEditor
        key={format}
        collection={collection}
        format={format}
        value={text}
        onChange={(next) => {
          setText(next);
          setServer([]);
          validate.reset();
        }}
        problems={server}
      />
      {parsed.error && <p role="alert">{parsed.error}</p>}
      <ProblemList problems={[...local, ...server]} />
      {validate.data?.valid && !server.length && (
        <p role="status">The manager accepts this definition.</p>
      )}
      {(validate.error || save.error) && (
        <ErrorNotice error={(save.error ?? validate.error)!} />
      )}
      <div className="actions">
        <button
          type="button"
          disabled={
            !parsed.value || validate.isPending || !session.data?.csrfToken
          }
          onClick={() => validate.mutate()}
        >
          Validate
        </button>
        <button
          type="button"
          disabled={
            !parsed.value ||
            errors.length > 0 ||
            save.isPending ||
            !session.data?.csrfToken
          }
          onClick={() => save.mutate()}
        >
          {version === undefined ? "Save as new draft" : "Save draft"}
        </button>
      </div>
      <p className="hint">
        Drafts may be incomplete. Publishing checks the definition against the
        installation's storage sources and admin secrets, then freezes it.
      </p>
    </section>
  );
}
