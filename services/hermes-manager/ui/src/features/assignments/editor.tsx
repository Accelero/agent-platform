import * as Dialog from "@radix-ui/react-dialog";
import { useId, useRef, useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import {
  ApiError,
  api,
  createIntent,
  displayProblems,
  type AssignmentSpec,
  type Identity,
  type Schemas,
} from "../../api";
import { queryClient, useActorKey, useSession } from "../../app/state";
import { VersionPicker } from "../../components/catalog-picker";
import { ProblemList, usePermitted } from "../../components/command";
import { ErrorNotice } from "../../components/data";

type Volume = Schemas["volume"];
type SecretReference = Schemas["secretReference"];
// The contract defaults `mode`; the generated type marks it required.
type Terminal = Partial<Schemas["TerminalGrant"]>;
const modes = [
  "per_session",
  "per_profile_on_demand",
  "per_profile_always_running",
] as const;
export const emptySpec = (): AssignmentSpec => ({
  access: "enabled",
  template: { id: "", version: 0 },
});
function useSources() {
  const actor = useActorKey();
  return useQuery({
    queryKey: [actor, "storage-sources"],
    queryFn: ({ signal }) => api.storageSources(signal),
    staleTime: 60_000,
  });
}
/** Edits one assignment spec; every part is optional except access and template. */
export function SpecEditor({
  value,
  onChange,
}: {
  value: AssignmentSpec;
  onChange: (value: AssignmentSpec) => void;
}) {
  const unique = useId();
  const sources = useSources();
  const [config, setConfig] = useState(() =>
    value.adminConfig ? JSON.stringify(value.adminConfig, null, 2) : "",
  );
  const [configError, setConfigError] = useState<string>();
  const volumes = Object.entries(value.volumes ?? {});
  const setVolumes = (entries: [string, Volume][]) =>
    onChange({
      ...value,
      volumes: entries.length ? Object.fromEntries(entries) : undefined,
    });
  const terminal: Terminal = value.terminal ?? {};
  const allowed = Object.entries(terminal.allowed ?? {});
  const setTerminal = (next: Terminal) =>
    onChange({
      ...value,
      terminal:
        next.mode || next.default || Object.keys(next.allowed ?? {}).length
          ? { ...next, mode: next.mode ?? "per_session" }
          : undefined,
    });
  const secrets = value.secrets ?? [];
  const setSecrets = (next: SecretReference[]) =>
    onChange({ ...value, secrets: next.length ? next : undefined });
  const kindOf = (source: string) => {
    const found = sources.data?.items.find((item) => item.id === source);
    return found?.storageClass ? "dynamic" : found ? "static" : undefined;
  };
  return (
    <>
      <div className="field">
        <label htmlFor={`${unique}-access`}>Access</label>
        <select
          id={`${unique}-access`}
          value={value.access}
          onChange={(event) =>
            onChange({
              ...value,
              access: event.target.value as AssignmentSpec["access"],
            })
          }
        >
          <option value="enabled">enabled</option>
          <option value="suspended">suspended — stop Hermes, keep data</option>
          <option value="revoked">revoked — remove workloads, keep data</option>
        </select>
      </div>
      <VersionPicker
        collection="templates"
        label="Hermes template"
        value={value.template.id ? value.template : undefined}
        onChange={(template) =>
          onChange({ ...value, template: template ?? { id: "", version: 0 } })
        }
      />
      <fieldset>
        <legend>Volumes</legend>
        <p className="hint">
          Volumes the template does not define come from its defaults. A volume
          shared with terminal backends is mounted at the same path in both.
        </p>
        {sources.error && <ErrorNotice error={sources.error} />}
        {volumes.map(([name, volume], index) => {
          const update = (next: [string, Volume]) =>
            setVolumes(volumes.map((entry, i) => (i === index ? next : entry)));
          const kind = kindOf(volume.source);
          return (
            <div className="array-entry" key={index}>
              <label>
                Volume name
                <input
                  required
                  maxLength={63}
                  value={name}
                  onChange={(event) => update([event.target.value, volume])}
                />
              </label>
              <label>
                Source
                <select
                  required
                  value={volume.source}
                  onChange={(event) =>
                    update([name, { source: event.target.value }])
                  }
                >
                  <option value="">Choose a source</option>
                  {sources.data?.items.map((source) => (
                    <option key={source.id} value={source.id}>
                      {source.id}
                    </option>
                  ))}
                </select>
              </label>
              {kind === "static" && (
                <label>
                  Path under the source root
                  <input
                    required
                    maxLength={1024}
                    value={volume.path ?? ""}
                    onChange={(event) =>
                      update([
                        name,
                        { ...volume, path: event.target.value || undefined },
                      ])
                    }
                  />
                </label>
              )}
              {kind === "dynamic" && (
                <label>
                  Size (GiB)
                  <input
                    type="number"
                    required
                    min={1}
                    value={volume.sizeGib ?? ""}
                    onChange={(event) =>
                      update([
                        name,
                        {
                          ...volume,
                          sizeGib: event.target.value
                            ? event.target.valueAsNumber
                            : undefined,
                        },
                      ])
                    }
                  />
                </label>
              )}
              <label>
                <input
                  type="checkbox"
                  checked={!!volume.readOnly}
                  onChange={(event) =>
                    update([
                      name,
                      { ...volume, readOnly: event.target.checked },
                    ])
                  }
                />
                Read-only
              </label>
              <button
                type="button"
                onClick={() =>
                  setVolumes(volumes.filter((_, i) => i !== index))
                }
              >
                Remove volume {name || index + 1}
              </button>
            </div>
          );
        })}
        <button
          type="button"
          disabled={volumes.length >= 16}
          onClick={() => setVolumes([...volumes, ["", { source: "" }]])}
        >
          Add volume
        </button>
      </fieldset>
      <fieldset>
        <legend>Terminal backends</legend>
        <div className="field">
          <label htmlFor={`${unique}-mode`}>Environment lifetime</label>
          <select
            id={`${unique}-mode`}
            value={terminal.mode ?? ""}
            onChange={(event) =>
              setTerminal({
                ...terminal,
                mode: (event.target.value || undefined) as Terminal["mode"],
              })
            }
          >
            <option value="">Default (per_session)</option>
            {modes.map((mode) => (
              <option key={mode}>{mode}</option>
            ))}
          </select>
        </div>
        {allowed.map(([id, version], index) => (
          <div className="array-entry" key={index}>
            <VersionPicker
              collection="terminal-backends"
              label={`Allowed backend ${index + 1}`}
              value={id ? { id, version } : undefined}
              onChange={(ref) =>
                setTerminal({
                  ...terminal,
                  allowed: Object.fromEntries(
                    allowed.map((entry, i) =>
                      i === index && ref ? [ref.id, ref.version] : entry,
                    ),
                  ),
                })
              }
            />
            <button
              type="button"
              onClick={() =>
                setTerminal({
                  ...terminal,
                  allowed: Object.fromEntries(
                    allowed.filter((_, i) => i !== index),
                  ),
                  default:
                    terminal.default === id ? undefined : terminal.default,
                })
              }
            >
              Remove backend {id || index + 1}
            </button>
          </div>
        ))}
        <button
          type="button"
          disabled={allowed.some(([id]) => !id) || allowed.length >= 16}
          onClick={() =>
            setTerminal({
              ...terminal,
              allowed: { ...terminal.allowed, "": 1 },
            })
          }
        >
          Allow a backend
        </button>
        <div className="field">
          <label htmlFor={`${unique}-default`}>Default backend</label>
          <select
            id={`${unique}-default`}
            value={terminal.default ?? ""}
            onChange={(event) =>
              setTerminal({
                ...terminal,
                default: event.target.value || undefined,
              })
            }
          >
            <option value="">None</option>
            {allowed
              .filter(([id]) => id)
              .map(([id]) => (
                <option key={id}>{id}</option>
              ))}
          </select>
        </div>
      </fieldset>
      <fieldset>
        <legend>Admin settings overrides</legend>
        <p className="hint">
          Layered over the template's admin settings. Never put secret values
          here; reference admin secrets instead.
        </p>
        <textarea
          aria-label="Admin settings overrides (JSON)"
          value={config}
          maxLength={8192}
          onChange={(event) => {
            const text = event.target.value;
            setConfig(text);
            if (!text.trim()) {
              setConfigError(undefined);
              onChange({ ...value, adminConfig: undefined });
              return;
            }
            try {
              const parsed: unknown = JSON.parse(text);
              if (
                !parsed ||
                typeof parsed !== "object" ||
                Array.isArray(parsed)
              )
                throw new Error();
              setConfigError(undefined);
              onChange({
                ...value,
                adminConfig: parsed as Record<string, never>,
              });
            } catch {
              setConfigError("Enter one JSON object.");
            }
          }}
        />
        {configError && <p role="alert">{configError}</p>}
      </fieldset>
      <fieldset>
        <legend>Secret references</legend>
        {secrets.map((secret, index) => {
          const update = (next: SecretReference) =>
            setSecrets(secrets.map((entry, i) => (i === index ? next : entry)));
          const delivery = secret.file !== undefined ? "file" : "env";
          return (
            <div className="array-entry" key={index}>
              <label>
                Secret name
                <input
                  required
                  maxLength={63}
                  value={secret.name}
                  onChange={(event) =>
                    update({ ...secret, name: event.target.value })
                  }
                />
              </label>
              <label>
                Delivery
                <select
                  value={delivery}
                  onChange={(event) =>
                    update(
                      event.target.value === "file"
                        ? { name: secret.name, file: "" }
                        : { name: secret.name, env: "" },
                    )
                  }
                >
                  <option value="env">Environment variable</option>
                  <option value="file">File</option>
                </select>
              </label>
              <label>
                {delivery === "env" ? "Variable" : "File name"}
                <input
                  required
                  maxLength={128}
                  value={(delivery === "env" ? secret.env : secret.file) ?? ""}
                  onChange={(event) =>
                    update(
                      delivery === "env"
                        ? { name: secret.name, env: event.target.value }
                        : { name: secret.name, file: event.target.value },
                    )
                  }
                />
              </label>
              <button
                type="button"
                onClick={() =>
                  setSecrets(secrets.filter((_, i) => i !== index))
                }
              >
                Remove secret {secret.name || index + 1}
              </button>
            </div>
          );
        })}
        <button
          type="button"
          disabled={secrets.length >= 32}
          onClick={() => setSecrets([...secrets, { name: "", env: "" }])}
        >
          Add secret reference
        </button>
      </fieldset>
    </>
  );
}
function EditorDialog({
  title,
  children,
}: {
  title: string;
  children: (close: () => void) => React.ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const cancel = useRef<HTMLButtonElement>(null);
  if (!usePermitted("administer")) return null;
  return (
    <Dialog.Root open={open} onOpenChange={setOpen}>
      <Dialog.Trigger asChild>
        <button>{title}</button>
      </Dialog.Trigger>
      <Dialog.Portal>
        <Dialog.Overlay className="dialog-overlay" />
        <Dialog.Content
          className="dialog-content wide"
          onOpenAutoFocus={(event) => {
            event.preventDefault();
            cancel.current?.focus();
          }}
        >
          <Dialog.Title>{title}</Dialog.Title>
          <Dialog.Description>
            The change is recorded first; the controller then applies it.
          </Dialog.Description>
          {open && children(() => setOpen(false))}
          <Dialog.Close asChild>
            <button ref={cancel} className="dialog-cancel">
              Cancel / close
            </button>
          </Dialog.Close>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
type Receipt = Schemas["AssignmentReceipt"];
/** Creates the one assignment of an exact identity. */
export function CreateAssignment({ identity }: { identity: Identity }) {
  return (
    <EditorDialog title="Create assignment">
      {() => <CreateBody identity={identity} />}
    </EditorDialog>
  );
}
function CreateBody({ identity }: { identity: Identity }) {
  const session = useSession();
  const [spec, setSpec] = useState(emptySpec);
  const [submit] = useState(() => createIntent());
  const mutation = useMutation({
    mutationFn: () =>
      submit<Receipt>(
        "POST",
        "/api/v1/assignments",
        { identity, spec },
        session.data!.csrfToken!,
      ),
    onSuccess: () => void queryClient.invalidateQueries(),
  });
  if (mutation.data)
    return (
      <p role="status">
        Assignment created.{" "}
        <Link to="/assignments/$id" params={{ id: mutation.data.assignmentId }}>
          Open the assignment
        </Link>
      </p>
    );
  return (
    <form
      onSubmit={(event) => {
        event.preventDefault();
        mutation.mutate();
      }}
    >
      <p>
        For <code>{identity.subject}</code> at <code>{identity.issuer}</code>.
      </p>
      <fieldset disabled={mutation.isPending}>
        <SpecEditor value={spec} onChange={setSpec} />
      </fieldset>
      {mutation.error && <ErrorNotice error={mutation.error} />}
      <ProblemList error={mutation.error} />
      <button
        disabled={
          mutation.isPending || !spec.template.id || !session.data?.csrfToken
        }
      >
        Confirm assignment
      </button>
    </form>
  );
}
/** Changes an assignment after a server preview of its effects. */
export function ChangeAssignment({
  id,
  spec,
  etag,
}: {
  id: string;
  spec: AssignmentSpec;
  etag: string | null;
}) {
  return (
    <EditorDialog title="Change assignment">
      {() => <ChangeBody id={id} current={spec} etag={etag} />}
    </EditorDialog>
  );
}
function ChangeBody({
  id,
  current,
  etag,
}: {
  id: string;
  current: AssignmentSpec;
  etag: string | null;
}) {
  const session = useSession();
  const [spec, setSpec] = useState(() => structuredClone(current));
  const [submit] = useState(() => createIntent());
  // Every key is sent: a change replaces each part it names.
  const changes = {
    access: spec.access,
    template: spec.template,
    volumes: spec.volumes ?? {},
    terminal: spec.terminal ?? { mode: "per_session" as const },
    adminConfig: spec.adminConfig ?? {},
    secrets: spec.secrets ?? [],
  };
  const preview = useMutation({
    mutationFn: () => api.preview(id, changes, session.data!.csrfToken!),
  });
  const mutation = useMutation({
    mutationFn: () =>
      submit<Receipt>(
        "PATCH",
        `/api/v1/assignments/${encodeURIComponent(id)}`,
        changes,
        session.data!.csrfToken!,
        etag,
      ),
    onSettled: () => void queryClient.invalidateQueries(),
  });
  const stale =
    mutation.error instanceof ApiError && mutation.error.status === 412;
  if (mutation.data)
    return (
      <p role="status">
        Change accepted as version {mutation.data.version}. The instance phase
        shows when it is applied.
      </p>
    );
  return (
    <form
      onSubmit={(event) => {
        event.preventDefault();
        mutation.mutate();
      }}
    >
      <fieldset disabled={mutation.isPending}>
        <SpecEditor
          value={spec}
          onChange={(next) => {
            setSpec(next);
            preview.reset();
          }}
        />
      </fieldset>
      <button
        type="button"
        disabled={preview.isPending || !session.data?.csrfToken}
        onClick={() => preview.mutate()}
      >
        Preview effects
      </button>
      {preview.error && <ErrorNotice error={preview.error} />}
      {preview.data && (
        <section className="panel detail" aria-label="Preview">
          <h2>{preview.data.valid ? "Valid change" : "Invalid change"}</h2>
          <ul>
            <li>
              {preview.data.restartsHermes
                ? "Restarts Hermes."
                : "Hermes keeps running."}
            </li>
            <li>
              {preview.data.recreatesTerminals
                ? "Recreates terminal environments."
                : "Terminal environments are kept."}
            </li>
          </ul>
          <ProblemList problems={displayProblems(preview.data)} />
        </section>
      )}
      {mutation.error && <ErrorNotice error={mutation.error} />}
      <ProblemList error={mutation.error} />
      {stale && <p>Close this dialog and reopen it on the refreshed record.</p>}
      <button
        disabled={
          mutation.isPending ||
          stale ||
          !preview.data?.valid ||
          !session.data?.csrfToken
        }
      >
        Confirm change
      </button>
    </form>
  );
}
