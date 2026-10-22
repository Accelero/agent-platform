import {
  parseConfiguration,
  serializeConfiguration,
  type ConfigurationFormat,
} from "./yaml";
import { ReferenceInput } from "../../components/reference";
import { Fields } from "../../components/command";
import { registry } from "../../schemas/generated/metadata";
import { lazy, Suspense, useEffect, useRef, useState } from "react";
import { useQuery, useMutation } from "@tanstack/react-query";
import {
  Link,
  useBlocker,
  useNavigate,
  useParams,
  useSearch,
} from "@tanstack/react-router";
import { useForm } from "react-hook-form";
import { ApiError, api, createIntent } from "../../api";
import type { ConfigurationProblem } from "../../api";
import type { components } from "../../generated/api";
import {
  queryClient,
  registerPrivateReset,
  useActorKey,
  useSession,
} from "../../app/state";
import { ErrorNotice, JsonDetails, ReadState } from "../../components/data";
import {
  changedPaths,
  initialDefinition,
  parseDraft,
  setPath,
  validateDraft,
} from "./model";
import type { Draft } from "./model";
const RawEditor = lazy(() => import("./raw-editor"));
const deployment = [
  "image",
  "storageClass",
  "homeGib",
  "workspaceGib",
  "cpuMillis",
  "memoryMib",
] as const;
const bounds: Record<string, [number, number]> = {
  homeGib: [1, 1048576],
  workspaceGib: [1, 1048576],
  cpuMillis: [10, 64000],
  memoryMib: [128, 262144],
};
export default function TemplateEditor() {
  const { id } = useParams({ strict: false });
  const search = useSearch({ strict: false }) as {
    cloneTemplate?: string;
    cloneVersion?: string;
    cloneDraft?: string;
    mode?: string;
  };
  const actor = useActorKey();
  const session = useSession();
  const navigate = useNavigate();
  const query = useQuery({
    queryKey: [actor, "draft", id],
    enabled: !!id,
    queryFn: ({ signal }) => api.draft(id!, signal),
  });
  const [templateId, setTemplateId] = useState("");
  const [savedTemplateId, setSavedTemplateId] = useState("");
  const [draft, setDraft] = useState<Draft>(initialDefinition);
  const [raw, setRaw] = useState(JSON.stringify(initialDefinition, null, 2));
  const [saved, setSaved] = useState(raw);
  const [etag, setEtag] = useState<string | null>();
  const [tab, setTab] = useState("Form");
  const [format, setFormat] = useState<ConfigurationFormat>("json");
  const [serverProblems, setServerProblems] = useState<ConfigurationProblem[]>(
    [],
  );
  const [publishReview, setPublishReview] = useState(false);
  const [published, setPublished] = useState<number | null>(null);
  const initialized = useRef<string | null>(null);
  const [saveIntent] = useState(() => createIntent());
  const [publishIntent] = useState(() => createIntent());
  const [validateIntent] = useState(() => createIntent());
  useEffect(() => {
    if (query.data && initialized.current !== id) {
      const value = query.data.data;
      setTemplateId(value.templateId);
      setSavedTemplateId(value.templateId);
      setDraft(value.definition as Draft);
      const text = JSON.stringify(value.definition, null, 2);
      setRaw(text);
      setFormat("json");
      setSaved(text);
      setEtag(query.data.etag);
      setPublished(value.publishedVersion);
      initialized.current = id!;
    }
  }, [query.data, id]);
  const clone = useQuery({
    queryKey: [
      actor,
      "clone-template",
      search.cloneTemplate,
      search.cloneVersion,
      search.cloneDraft,
    ],
    enabled: !id && !!(search.cloneTemplate || search.cloneDraft),
    queryFn: async ({ signal }) =>
      search.cloneDraft
        ? (await api.draft(search.cloneDraft, signal)).data
        : api.templateVersion(
            search.cloneTemplate!,
            Number(search.cloneVersion),
            signal,
          ),
  });
  const cloneKey = JSON.stringify([
    search.cloneTemplate,
    search.cloneVersion,
    search.cloneDraft,
    search.mode,
  ]);
  const cloned = useRef<string | null>(null);
  useEffect(() => {
    if (clone.data && cloned.current !== cloneKey && !id) {
      cloned.current = cloneKey;
      setDraft(clone.data.definition as Draft);
      setRaw(JSON.stringify(clone.data.definition, null, 2));
      if (search.mode === "revision") setTemplateId(search.cloneTemplate!);
    }
  }, [clone.data, id, search.mode, search.cloneTemplate, cloneKey]);
  const capabilities = useQuery({
    queryKey: [actor, "configuration-schemas"],
    queryFn: ({ signal }) => api.schemas(signal),
  });
  const clientCompatible =
    !capabilities.data?.schemas ||
    registry.every((local) =>
      capabilities.data!.schemas.some(
        (remote) => remote.id === local.id && remote.digest === local.digest,
      ),
    );
  const parsed = parseConfiguration(raw, format);
  const dirty =
    !!parsed.error ||
    JSON.stringify(parsed.value) !== JSON.stringify(parseDraft(saved).value) ||
    templateId !== savedTemplateId;
  const dirtyRef = useRef(false);
  dirtyRef.current = dirty;
  useEffect(
    () =>
      registerPrivateReset(() => {
        dirtyRef.current = false;
        setTemplateId("");
        setSavedTemplateId("");
        setDraft(initialDefinition);
        setFormat("json");
        setRaw(JSON.stringify(initialDefinition, null, 2));
        setSaved(JSON.stringify(initialDefinition, null, 2));
        setServerProblems([]);
      }),
    [],
  );
  const blocker = useBlocker({
    shouldBlockFn: () => dirtyRef.current,
    enableBeforeUnload: () => dirtyRef.current,
    withResolver: true,
  });
  const problems = parsed.error
    ? [
        {
          pointer: "",
          code: "syntax",
          severity: "error" as const,
          message: parsed.error,
        },
      ]
    : [...(clientCompatible ? validateDraft(draft) : []), ...serverProblems];
  const form = useForm<Draft>();
  useEffect(() => form.reset(draft), [draft, form]);
  function update(next: Draft) {
    setDraft(next);
    setRaw(serializeConfiguration(next, format));
    setServerProblems([]);
    setPublishReview(false);
  }
  const save = useMutation({
    mutationFn: async () => {
      await validateIntent(
        "POST",
        "/api/v1/template-definitions/validate",
        draft,
        session.data!.csrfToken!,
      );
      return (await saveIntent(
        id ? "PATCH" : "POST",
        id ? `/api/v1/template-drafts/${id}` : "/api/v1/template-drafts",
        { templateId, definition: draft },
        session.data!.csrfToken!,
        id ? etag : undefined,
      )) as components["schemas"]["TemplateDraftAccepted"];
    },
    onSuccess: (result) => {
      const text = JSON.stringify(result.draft.definition, null, 2);
      setSaved(text);
      setSavedTemplateId(result.draft.templateId);
      setRaw(serializeConfiguration(result.draft.definition as Draft, format));
      setDraft(result.draft.definition as Draft);
      setEtag(`"${result.draft.version}"`);
      dirtyRef.current = false;
      void queryClient.invalidateQueries();
      if (!id)
        void navigate({
          to: "/template-drafts/$id",
          params: { id: result.draft.id },
        });
    },
    onError: (error) => {
      if (error instanceof ApiError) {
        setServerProblems(error.problems);
        if (error.status === 412) void query.refetch();
      }
    },
  });
  const publish = useMutation({
    mutationFn: () =>
      publishIntent(
        "POST",
        `/api/v1/template-drafts/${id}/publish`,
        {},
        session.data!.csrfToken!,
        etag,
      ) as Promise<components["schemas"]["TemplateDraftAccepted"]>,
    onSuccess: (result) => {
      setPublished(result.draft.publishedVersion);
      setEtag(`"${result.draft.version}"`);
      setPublishReview(false);
      void queryClient.invalidateQueries();
    },
    onError: (error) => {
      if (error instanceof ApiError) {
        setServerProblems(error.problems);
        if (error.status === 412) {
          setPublishReview(false);
          void query.refetch();
        }
      }
    },
  });
  const allowed = session.data?.permittedActions.includes(
    id ? "template.draft.update" : "template.draft.create",
  );
  const previous = useQuery({
    queryKey: [actor, "template-definition", templateId],
    enabled: !!id,
    queryFn: ({ signal }) => api.templateDefinition(templateId, signal),
  });
  const config = (draft.adminConfig ?? {}) as Draft;
  const setOverride = (path: string[], mode: string) =>
    update(
      setPath(
        draft,
        path,
        mode === "null" ? null : path.at(-1) === "max_turns" ? 1 : "",
        mode === "missing",
      ),
    );
  return (
    <ReadState
      pending={!!id && query.isPending}
      error={query.error}
      stale={query.isRefetchError}
    >
      <Link to="/templates">← Template definitions</Link>
      <h2>{id ? `Draft: ${templateId}` : "Create template draft"}</h2>
      <p>
        Saving a draft and publishing a pinned version do not restart or update
        assigned users. Apply is a separate reviewed command.
      </p>
      {blocker.status === "blocked" && (
        <div role="alert" className="error">
          Discard unsaved changes?
          <button onClick={() => blocker.reset()}>Keep editing</button>
          <button
            onClick={() => {
              dirtyRef.current = false;
              blocker.proceed();
            }}
          >
            Discard and leave
          </button>
        </div>
      )}
      {published && (
        <p role="status">
          Published immutable version {published}. Create a new draft for
          further edits.
        </p>
      )}
      <p>
        Configuration schema revision 1 covers reviewed terminal/plugin keys.
        Other managed keys are preserved and unvalidated. Image compatibility is
        tied to pinned image digests; unknown images have envelope and
        mandatory-policy coverage only.{" "}
        {!clientCompatible &&
          "This server schema revision has no bundled browser validator; saving uses authoritative server validation."}
      </p>
      {clone.error && <ErrorNotice error={clone.error} />}
      {clone.data && (
        <p>
          Copied configuration only. Saving creates a separate manager-owned
          draft; no assignment is changed.
        </p>
      )}
      <p>
        Selected image coverage:{" "}
        {registry.some((schema) =>
          schema.compatibleImageDigests.some((digest) =>
            String(draft.image ?? "").endsWith(`@${digest}`),
          ),
        )
          ? "reviewed terminal/plugin keys"
          : "envelope and mandatory platform policy; runtime-specific settings remain unvalidated"}
        .
      </p>
      <label>
        Template ID
        <input
          value={templateId}
          required
          pattern="[a-z][a-z0-9-]{0,62}"
          disabled={!!id || search.mode === "revision"}
          onChange={(event) => setTemplateId(event.target.value)}
        />
      </label>
      <div className="actions">
        {["Form", "JSON", "YAML"].map((name) => (
          <button
            key={name}
            aria-pressed={tab === name}
            disabled={name !== tab && !!parsed.error}
            onClick={() => {
              if (name !== "Form") {
                const nextFormat = name === "YAML" ? "yaml" : "json";
                if (nextFormat !== format)
                  setRaw(
                    serializeConfiguration(parsed.value ?? draft, nextFormat),
                  );
                setFormat(nextFormat);
              }
              setTab(name);
            }}
          >
            {name}
          </button>
        ))}
      </div>
      <fieldset
        disabled={
          !allowed || !!published || save.isPending || publish.isPending
        }
      >
        {tab !== "Form" ? (
          <Suspense fallback={<p>Loading editor…</p>}>
            <RawEditor
              value={raw}
              format={format}
              clientCompatible={clientCompatible}
              problems={serverProblems}
              onChange={(text) => {
                setRaw(text);
                setServerProblems([]);
                setPublishReview(false);
                const parsed = parseConfiguration(text, format);
                if (parsed.value) setDraft(parsed.value);
              }}
            />
          </Suspense>
        ) : (
          <form onSubmit={(event) => event.preventDefault()}>
            <h3>Deployment settings</h3>
            {deployment.map((key) => (
              <label key={key}>
                {key}
                {key === "storageClass" ? (
                  <ReferenceInput
                    kind="storageClass"
                    value={String(draft.storageClass ?? "")}
                    onChange={(v) =>
                      update(setPath(draft, ["storageClass"], v))
                    }
                  />
                ) : (
                  <input
                    {...form.register(key, {
                      onChange: (event) =>
                        update(
                          setPath(
                            draft,
                            [key],
                            bounds[key]
                              ? Number(event.target.value)
                              : event.target.value,
                          ),
                        ),
                    })}
                    required
                    type={bounds[key] ? "number" : "text"}
                    min={bounds[key]?.[0]}
                    max={bounds[key]?.[1]}
                  />
                )}
                {problems
                  .filter((problem) => problem.pointer === `/${key}`)
                  .map((problem, index) => (
                    <span role="status" key={index}>
                      {problem.message}
                    </span>
                  ))}
              </label>
            ))}
            <h3>Managed Hermes configuration</h3>
            <p>
              Only selected overrides replace retained user settings. Advanced
              fields remain in the JSON draft.
            </p>
            {[
              ["model"],
              ["terminal", "runtime_id"],
              ["agent", "max_turns"],
            ].map((path) => {
              let value: unknown = config;
              for (const key of path)
                value =
                  value && typeof value === "object"
                    ? (value as Draft)[key]
                    : undefined;
              const mode =
                value === undefined
                  ? "missing"
                  : value === null
                    ? "null"
                    : "value";
              return (
                <div key={path.join(".")}>
                  <label>
                    {path.join(".")} override
                    <select
                      value={mode}
                      onChange={(event) =>
                        setOverride(
                          ["adminConfig", ...path],
                          event.target.value,
                        )
                      }
                    >
                      <option value="missing">Leave to user</option>
                      <option value="null">Managed null</option>
                      <option value="value">Managed value</option>
                    </select>
                  </label>
                  {mode === "value" && (
                    <label>
                      {path.join(".")}
                      {path.join(".") === "terminal.runtime_id" &&
                      typeof value === "string" ? (
                        <ReferenceInput
                          kind="runtime"
                          value={value}
                          onChange={(v) =>
                            update(setPath(draft, ["adminConfig", ...path], v))
                          }
                        />
                      ) : (
                        <input
                          type={
                            path.at(-1) === "max_turns" &&
                            typeof value === "number"
                              ? "number"
                              : "text"
                          }
                          value={
                            typeof value === "string"
                              ? value
                              : JSON.stringify(value)
                          }
                          readOnly={
                            typeof value !== "string" &&
                            typeof value !== "number"
                          }
                          onChange={(event) =>
                            update(
                              setPath(
                                draft,
                                ["adminConfig", ...path],
                                typeof value === "number"
                                  ? Number(event.target.value)
                                  : event.target.value,
                              ),
                            )
                          }
                        />
                      )}
                    </label>
                  )}
                </div>
              );
            })}
            <p>
              Agent settings without reviewed schema coverage are preserved as
              runtime-specific values; server policy remains authoritative.
            </p>
            <Fields
              fields={[
                {
                  key: "plugins",
                  label: "Plugin settings",
                  fields: [
                    {
                      key: "enabled",
                      label: "Enabled plugin",
                      max: 64,
                      item: { key: "", label: "Plugin ID" },
                      initial: "",
                    },
                    {
                      key: "disabled",
                      label: "Disabled plugin",
                      max: 64,
                      item: { key: "", label: "Plugin ID" },
                      initial: "",
                    },
                  ],
                },
              ]}
              value={config}
              onChange={(next) => update(setPath(draft, ["adminConfig"], next))}
            />
            <h3>Platform enforced settings</h3>
            <p>
              Terminal backend: managed_execution. Required image-owned plugin:
              enabled. Authentication, non-root execution and credential
              delivery are enforced independently. Choosing a preferred runtime
              does not grant access to it.
            </p>
            <JsonDetails title="Complete managed overrides" value={config} />
          </form>
        )}
      </fieldset>
      <div aria-live="polite">
        {problems.length > 0 && (
          <ul>
            {problems.map((problem, index) => (
              <li key={index}>
                <code>{problem.pointer || "/"}</code>: {problem.message} (
                {problem.code})
              </li>
            ))}
          </ul>
        )}
      </div>
      {save.error && <ErrorNotice error={save.error} />}
      {publish.error && <ErrorNotice error={publish.error} />}
      {save.error instanceof ApiError && save.error.status === 412 && (
        <>
          <JsonDetails title="Current saved draft" value={query.data?.data} />
          <button
            onClick={() => {
              setEtag(query.data?.etag);
              save.reset();
            }}
          >
            Use refreshed version after reviewing the draft
          </button>
        </>
      )}
      {publish.error instanceof ApiError && publish.error.status === 412 && (
        <>
          <JsonDetails title="Draft reviewed for publication" value={draft} />
          <JsonDetails title="Current saved draft" value={query.data?.data} />
          <button
            disabled={!query.data || query.isFetching}
            onClick={() => {
              const current = query.data!.data;
              const text = JSON.stringify(current.definition, null, 2);
              setTemplateId(current.templateId);
              setSavedTemplateId(current.templateId);
              setDraft(current.definition as Draft);
              setRaw(
                serializeConfiguration(current.definition as Draft, format),
              );
              setSaved(text);
              setEtag(query.data!.etag);
              setPublished(current.publishedVersion);
              setServerProblems([]);
              setPublishReview(false);
              publish.reset();
              save.reset();
            }}
          >
            Load current draft for a new publication review
          </button>
        </>
      )}
      <div className="actions">
        <button
          disabled={
            !allowed ||
            !!published ||
            save.isPending ||
            !!parsed.error ||
            problems.some((item) => item.severity === "error") ||
            !templateId
          }
          onClick={() => save.mutate()}
        >
          Save draft
        </button>
        <button
          disabled={!dirty}
          onClick={() => {
            setTemplateId(savedTemplateId);
            setRaw(
              serializeConfiguration(
                parseDraft(saved).value ?? initialDefinition,
                format,
              ),
            );
            const parsed = parseDraft(saved);
            if (parsed.value) setDraft(parsed.value);
            setServerProblems([]);
          }}
        >
          Discard changes
        </button>
        {id && session.data?.permittedActions.includes("template.publish") && (
          <button
            disabled={
              dirty ||
              !!published ||
              !!parsed.error ||
              (publish.error instanceof ApiError &&
                publish.error.status === 412)
            }
            onClick={() => setPublishReview(true)}
          >
            Review publication
          </button>
        )}
      </div>
      {save.isSuccess && <p role="status">Draft saved.</p>}
      {publishReview && (
        <section className="panel detail">
          <h3>Publish immutable version</h3>
          <p>
            This records a new approved version. Existing assignments stay
            pinned. No user rollout is included.
          </p>
          <p>
            Current assigned-user count:{" "}
            {previous.data?.assignedUserCount ?? "unavailable"}. Publishing does
            not apply changes to these users.
          </p>
          <JsonDetails
            title="Changed setting paths"
            value={changedPaths(previous.data?.definition, draft)}
          />
          <JsonDetails
            title="Current published definition"
            value={previous.data?.definition}
          />
          <JsonDetails title="Definition to publish" value={draft} />
          <button disabled={publish.isPending} onClick={() => publish.mutate()}>
            Publish version
          </button>
          <button onClick={() => setPublishReview(false)}>
            Cancel publication
          </button>
        </section>
      )}
    </ReadState>
  );
}
