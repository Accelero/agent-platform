import * as Dialog from "@radix-ui/react-dialog";
import { useId, useRef, useState } from "react";
import type { ReactNode } from "react";
import { useMutation } from "@tanstack/react-query";
import {
  ApiError,
  createIntent,
  type ConfigurationProblem,
  type Method,
} from "../api";
import { queryClient, useSession } from "../app/state";
import { ErrorNotice, JsonDetails } from "./data";
import { VersionPicker } from "./catalog-picker";
import type { CatalogCollection } from "../api";

export type Permission = "administer" | "administrator.manage";
export type Field = {
  key: string;
  label: string;
  type?: "text" | "number" | "checkbox" | "password" | "textarea";
  min?: number;
  max?: number;
  options?: string[];
  fields?: Field[];
  /** A published catalog version, stored as `{id, version}`. */
  catalog?: CatalogCollection;
  optional?: boolean;
  readOnly?: boolean;
  hint?: string;
};
export function usePermitted(permission: Permission) {
  const session = useSession();
  return !!session.data?.permittedActions.includes(permission);
}
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
          if (field.optional && (next === "" || next === undefined))
            delete updated[field.key];
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
        if (field.catalog)
          return (
            <VersionPicker
              key={field.key}
              collection={field.catalog}
              label={label}
              value={item as { id: string; version: number } | undefined}
              onChange={update}
            />
          );
        const id = `${unique}-${field.key}`;
        return (
          <div className="field" key={field.key}>
            <label htmlFor={id}>{label}</label>
            {field.options ? (
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
            ) : field.type === "textarea" ? (
              <textarea
                id={id}
                required={!field.optional}
                readOnly={field.readOnly}
                maxLength={8192}
                value={String(item ?? "")}
                onChange={(event) => update(event.target.value)}
              />
            ) : (
              <input
                id={id}
                type={field.type ?? "text"}
                autoComplete={field.type === "password" ? "off" : undefined}
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
            {field.hint && <small>{field.hint}</small>}
          </div>
        );
      })}
    </>
  );
}
export function ProblemList({
  error,
  problems = error instanceof ApiError ? error.problems : [],
}: {
  error?: unknown;
  problems?: ConfigurationProblem[];
}) {
  if (!problems.length) return null;
  return (
    <ul className="problems" aria-label="Configuration problems">
      {problems.map((problem) => (
        <li key={`${problem.pointer}-${problem.code}`}>
          <code>{problem.pointer || "/"}</code> {problem.message}{" "}
          <span className="pill">{problem.code}</span>
        </li>
      ))}
    </ul>
  );
}
type CommandProps = {
  title: string;
  permission: Permission;
  method: Method;
  path: string;
  initial?: Record<string, unknown>;
  fields?: Field[];
  /** The record's ETag; required for changes to an existing record. */
  etag?: string | null;
  current?: unknown;
  warning?: string;
  /** Replaces the form; `draft` is still what is sent. */
  children?: ReactNode;
  renderReview?: (body: Record<string, unknown>) => ReactNode;
  transform?: (body: Record<string, unknown>) => unknown;
  /** Body-less commands (publish, retry, delete) send nothing. */
  bodyless?: boolean;
  onAccepted?: (receipt: unknown) => ReactNode;
  /** Values that must never stay in memory after the dialog closes. */
  sensitive?: boolean;
};
export function Command(props: CommandProps) {
  const permitted = usePermitted(props.permission);
  const [open, setOpen] = useState(false);
  const cancel = useRef<HTMLButtonElement>(null);
  if (!permitted) return null;
  return (
    <Dialog.Root open={open} onOpenChange={setOpen}>
      <Dialog.Trigger asChild>
        <button>{props.title}</button>
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
          <Dialog.Title>{props.title}</Dialog.Title>
          <Dialog.Description>
            {props.warning ??
              "Review the exact references and values before submitting. Changes are authorized and audited by the manager."}
          </Dialog.Description>
          {open && <CommandBody key={props.path} {...props} />}
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
  initial = {},
  fields = [],
  method,
  path,
  etag,
  current,
  children,
  renderReview,
  transform,
  bodyless,
  onAccepted,
  sensitive,
}: CommandProps) {
  const session = useSession();
  const [draft, setDraft] = useState(() => structuredClone(initial));
  const [submit] = useState(() => createIntent());
  const [version, setVersion] = useState(etag);
  const mutation = useMutation({
    mutationFn: () =>
      submit(
        method,
        path,
        bodyless ? undefined : transform ? transform(draft) : draft,
        session.data!.csrfToken!,
        version,
      ),
    onSuccess: () => {
      if (sensitive) setDraft({});
      void queryClient.invalidateQueries();
    },
    onError: (error) => {
      if (error instanceof ApiError && error.status === 412)
        void queryClient.invalidateQueries();
    },
  });
  const conflict =
    mutation.error instanceof ApiError && mutation.error.status === 412;
  if (mutation.isSuccess)
    return (
      <>
        <p role="status">
          Change accepted. The manager recorded it; reconciliation may still be
          in progress.
        </p>
        {onAccepted?.(mutation.data)}
      </>
    );
  return (
    <form
      onSubmit={(event) => {
        event.preventDefault();
        mutation.mutate();
      }}
    >
      <fieldset disabled={mutation.isPending}>
        <Fields fields={fields} value={draft} onChange={setDraft} />
        {children}
        {renderReview?.(draft)}
      </fieldset>
      {mutation.error && <ErrorNotice error={mutation.error} />}
      <ProblemList error={mutation.error} />
      {conflict && !sensitive && (
        <>
          <JsonDetails title="Submitted values" value={draft} />
          <JsonDetails title="Current record after refresh" value={current} />
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
        disabled={mutation.isPending || conflict || !session.data?.csrfToken}
      >
        Confirm change
      </button>
    </form>
  );
}
