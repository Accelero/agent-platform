import { Link } from "@tanstack/react-router";
import { TemplatePicker } from "./template-picker";
import { ReferenceInput, type ReferenceKind } from "./reference";
import * as Dialog from "@radix-ui/react-dialog";
import { useEffect, useId, useState, useRef } from "react";
import type { ReactNode } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { ApiError, api, createIntent } from "../api";
import { queryClient, useActorKey, useSession } from "../app/state";
import { ErrorNotice, JsonDetails, StatusBadge } from "./data";

export type Field = {
  template?: boolean;
  reference?: ReferenceKind;
  referenceInstanceId?: string;
  versionKey?: string;
  key: string;
  label: string;
  type?: "text" | "number" | "checkbox";
  min?: number;
  max?: number;
  options?: string[];
  fields?: Field[];
  item?: Field;
  initial?: unknown;
  optional?: boolean;
  readOnly?: boolean;
};
export function Fields({
  fields,
  value,
  onChange,
  prefix = "",
}: {
  fields: Field[];
  value: Record<string, unknown>;
  onChange: (value: Record<string, unknown>) => void;
  prefix?: string;
}) {
  const unique = useId();
  return (
    <>
      {fields.map((field) => {
        const item = value[field.key];
        const label = `${prefix}${field.label}`;
        const update = (next: unknown) => {
          const updated = { ...value, [field.key]: next };
          if (field.optional && next === "") delete updated[field.key];
          onChange(updated);
        };
        if (field.fields)
          return (
            <fieldset key={field.key}>
              <legend>{label}</legend>
              <Fields
                fields={field.fields}
                value={(item ?? {}) as Record<string, unknown>}
                onChange={update}
                prefix={`${label}: `}
              />
            </fieldset>
          );
        if (field.item) {
          const items = (item ?? []) as unknown[];
          return (
            <fieldset key={field.key}>
              <legend>{label}</legend>
              {items.map((entry, index) => (
                <div className="array-entry" key={index}>
                  <Fields
                    fields={[
                      {
                        ...field.item!,
                        key: "entry",
                        label: `${field.label} ${index + 1}`,
                      },
                    ]}
                    prefix={prefix}
                    value={{ entry }}
                    onChange={(next) =>
                      update(
                        items.map((current, i) =>
                          i === index ? next.entry : current,
                        ),
                      )
                    }
                  />
                  <button
                    type="button"
                    onClick={() => update(items.filter((_, i) => i !== index))}
                  >
                    Remove {field.label} {index + 1}
                  </button>
                </div>
              ))}
              <button
                type="button"
                disabled={items.length >= (field.max ?? 16)}
                onClick={() =>
                  update([...items, structuredClone(field.initial ?? {})])
                }
              >
                Add {field.label}
              </button>
            </fieldset>
          );
        }
        if (field.template)
          return (
            <TemplatePicker
              key={field.key}
              value={String(item ?? "")}
              onChange={update}
              label={label}
            />
          );
        const id = `${unique}-${field.key}`;
        return (
          <div className="field" key={field.key}>
            <label htmlFor={id}>{label}</label>
            {field.reference ? (
              <ReferenceInput
                id={id}
                kind={field.reference}
                value={String(item ?? "")}
                onChange={update}
                instanceId={field.referenceInstanceId}
                readOnly={field.readOnly}
                required={!field.optional}
                onSelect={(choice) =>
                  onChange({
                    ...value,
                    [field.key]: choice.id,
                    ...(field.versionKey && choice.version
                      ? { [field.versionKey]: choice.version }
                      : {}),
                  })
                }
              />
            ) : field.options ? (
              <select
                id={id}
                required={!field.optional}
                value={String(item ?? "")}
                disabled={field.readOnly}
                onChange={(event) => update(event.target.value)}
              >
                {field.optional && <option value="">Not set</option>}
                {field.options.map((option) => (
                  <option key={option}>{option}</option>
                ))}
              </select>
            ) : (
              <input
                id={id}
                type={field.type ?? "text"}
                required={!field.optional && field.type !== "checkbox"}
                readOnly={field.readOnly}
                min={field.min}
                max={field.max}
                maxLength={field.type === "number" ? undefined : 2048}
                value={
                  field.type === "checkbox" ? undefined : String(item ?? "")
                }
                checked={field.type === "checkbox" ? Boolean(item) : undefined}
                onChange={(event) =>
                  update(
                    field.type === "checkbox"
                      ? event.target.checked
                      : field.type === "number"
                        ? event.target.value === ""
                          ? undefined
                          : event.target.valueAsNumber
                        : event.target.value,
                  )
                }
              />
            )}
          </div>
        );
      })}
    </>
  );
}
export const terminalOperation = (state?: string) =>
  !!state && ["Succeeded", "Blocked", "Failed", "Superseded"].includes(state);
export function pollInterval(
  started: number,
  now: number,
  count: number,
  state?: string,
) {
  return terminalOperation(state) || now - started >= 600_000
    ? false
    : Math.min(1000 * 2 ** Math.min(count, 4), 10_000);
}
function OperationProgress({ id, started }: { id: string; started: number }) {
  const actor = useActorKey();
  const [expired, setExpired] = useState(false);
  const query = useQuery({
    queryKey: [actor, "operation", id],
    queryFn: ({ signal }) => api.operation(id, signal),
    refetchOnWindowFocus: true,
    enabled: !expired,
    refetchInterval: (query) =>
      pollInterval(
        started,
        Date.now(),
        query.state.dataUpdateCount,
        query.state.data?.state,
      ),
  });
  useEffect(() => {
    const timer = setTimeout(
      () => setExpired(true),
      Math.max(0, 600_000 - (Date.now() - started)),
    );
    return () => clearTimeout(timer);
  }, [started]);
  useEffect(() => {
    if (terminalOperation(query.data?.state))
      void queryClient.invalidateQueries({
        predicate: (entry) => entry.queryKey[1] !== "operation",
      });
  }, [query.data?.state]);
  return (
    <div role="status">
      <p>Operation {id}</p>
      {query.data && <StatusBadge value={query.data.state} />}
      <p>
        {expired
          ? "Automatic tracking stopped after 10 minutes. Refresh the instance to check progress."
          : "Accepted commands may still be waiting for reconciliation."}
      </p>
      {query.error && <ErrorNotice error={query.error} />}
    </div>
  );
}
export function Command({
  title,
  permission,
  method,
  path,
  initial,
  fields = [],
  etag,
  current,
  warning,
  children,
  renderReview,
  preflight,
  transform,
}: {
  title: string;
  permission: string;
  method: "POST" | "PATCH";
  path: string;
  initial: Record<string, unknown>;
  fields?: Field[];
  etag?: string | null;
  current?: unknown;
  warning?: string;
  children?: ReactNode;
  renderReview?: (body: Record<string, unknown>) => ReactNode;
  preflight?: (body: Record<string, unknown>) => Promise<void>;
  transform?: (body: Record<string, unknown>) => unknown;
}) {
  const session = useSession();
  const [open, setOpen] = useState(false);
  const cancel = useRef<HTMLButtonElement>(null);
  if (!session.data?.permittedActions.some((action) => action === permission))
    return null;
  return (
    <Dialog.Root open={open} onOpenChange={setOpen}>
      <Dialog.Trigger asChild>
        <button>{title}</button>
      </Dialog.Trigger>
      <Dialog.Portal>
        <Dialog.Overlay className="dialog-overlay" />
        <Dialog.Content
          className="dialog-content"
          onOpenAutoFocus={(event) => {
            event.preventDefault();
            cancel.current?.focus();
          }}
        >
          <Dialog.Title>{title}</Dialog.Title>
          <Dialog.Description>
            {warning ??
              "Review the exact references and values before submitting. Changes are authorized and audited by the manager."}
          </Dialog.Description>
          {open && (
            <CommandBody
              key={`${path}-${open}`}
              {...{
                initial,
                fields,
                method,
                path,
                etag,
                current,
                children,
                renderReview,
                preflight,
                transform,
              }}
            />
          )}
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
function CommandBody({
  initial,
  fields,
  method,
  path,
  etag,
  current,
  children,
  renderReview,
  preflight,
  transform,
}: Omit<Parameters<typeof Command>[0], "title" | "permission">) {
  const session = useSession();
  const [draft, setDraft] = useState(() => structuredClone(initial));
  const [submit] = useState(() => createIntent());
  const [version, setVersion] = useState(etag);
  const [started, setStarted] = useState(0);
  const mutation = useMutation({
    mutationFn: async () => {
      if (preflight) await preflight(draft);
      return submit(
        method,
        path,
        transform ? transform(draft) : draft,
        session.data!.csrfToken!,
        version,
      );
    },
    onSuccess: () => {
      setStarted(Date.now());
      void queryClient.invalidateQueries();
    },
    onError: (error) => {
      if (error instanceof ApiError && error.status === 412)
        void queryClient.invalidateQueries();
    },
  });
  const conflict =
    mutation.error instanceof ApiError && mutation.error.status === 412;
  const accepted = mutation.data as
    { operationId?: string; eventId?: string } | undefined;
  return (
    <>
      {mutation.isSuccess ? (
        <>
          <p role="status">Change accepted.</p>
          {accepted?.operationId ? (
            <OperationProgress id={accepted.operationId} started={started} />
          ) : (
            <p>
              The configuration event was recorded.{" "}
              {accepted?.eventId && (
                <Link to="/activity/$id" params={{ id: accepted.eventId }}>
                  Inspect recorded configuration change
                </Link>
              )}
              . Refresh the resource to inspect its applied state.
            </p>
          )}
        </>
      ) : (
        <form
          onSubmit={(event) => {
            event.preventDefault();
            mutation.mutate();
          }}
        >
          <fieldset disabled={mutation.isPending}>
            <Fields fields={fields ?? []} value={draft} onChange={setDraft} />
            {children}
            {renderReview?.(draft)}
          </fieldset>
          {mutation.error && <ErrorNotice error={mutation.error} />}{" "}
          {conflict && (
            <>
              <JsonDetails title="Submitted values" value={draft} />
              <JsonDetails
                title="Current record after refresh"
                value={current}
              />
              <button
                type="button"
                onClick={() => {
                  setVersion(etag);
                  mutation.reset();
                }}
              >
                Use refreshed version after review
              </button>
            </>
          )}
          <button
            disabled={
              mutation.isPending || conflict || !session.data?.csrfToken
            }
          >
            Confirm change
          </button>
        </form>
      )}
    </>
  );
}
