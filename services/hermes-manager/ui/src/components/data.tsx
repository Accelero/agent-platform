import type { ReactNode } from "react";
import { useEffect, useState } from "react";
import {
  flexRender,
  getCoreRowModel,
  useReactTable,
} from "@tanstack/react-table";
import type { ColumnDef } from "@tanstack/react-table";
import { useLocation, useNavigate } from "@tanstack/react-router";
export function ErrorNotice({ error }: { error: Error }) {
  return (
    <div role="alert" className="error">
      {error.message}
    </div>
  );
}
export function StatusBadge({ value }: { value: string }) {
  return <span className={`pill status-${value.toLowerCase()}`}>{value}</span>;
}
export function CopyId({ value }: { value: string }) {
  const [copied, setCopied] = useState(false);
  const [failed, setFailed] = useState(false);
  return (
    <span className="id-cell">
      <code>{value}</code>
      <button
        aria-label={`Copy ${value}`}
        onClick={() => {
          navigator.clipboard.writeText(value).then(
            () => setCopied(true),
            () => setFailed(true),
          );
        }}
      >
        {copied ? "Copied" : "Copy"}
      </button>
      {failed && <span role="status">Select and copy the ID manually.</span>}
    </span>
  );
}
export function DataTable<T>({
  caption,
  rows,
  columns,
  empty,
}: {
  caption: string;
  rows: T[];
  columns: ColumnDef<T>[];
  empty: string;
}) {
  const table = useReactTable({
    data: rows,
    columns,
    getCoreRowModel: getCoreRowModel(),
    manualSorting: true,
    manualPagination: true,
  });
  return (
    <section className="panel">
      <div className="panel-heading">
        <h2>{caption}</h2>
        <span>{rows.length} on this page</span>
      </div>
      {rows.length ? (
        <div className="table-scroll">
          <table>
            <caption className="sr-only">{caption}</caption>
            <thead>
              {table.getHeaderGroups().map((group) => (
                <tr key={group.id}>
                  {group.headers.map((header) => (
                    <th scope="col" key={header.id}>
                      {flexRender(
                        header.column.columnDef.header,
                        header.getContext(),
                      )}
                    </th>
                  ))}
                </tr>
              ))}
            </thead>
            <tbody>
              {table.getRowModel().rows.map((row) => (
                <tr key={row.id}>
                  {row.getVisibleCells().map((cell) => (
                    <td key={cell.id}>
                      {flexRender(
                        cell.column.columnDef.cell,
                        cell.getContext(),
                      )}
                    </td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : (
        <div className="empty">
          <span>↳</span>
          <p>{empty}</p>
        </div>
      )}
    </section>
  );
}
export function validatePage(search: Record<string, unknown>): {
  cursors?: string[];
  pageOffset?: number;
  instanceId?: string;
  q?: string;
  sort?: string;
  access?: string;
  state?: string;
  templateId?: string;
  action?: string;
  actor?: string;
  source?: string;
  assigned?: string;
  identityId?: string;
  runtimeId?: string;
  runtimeVersion?: string;
  publication?: string;
  collection?: string;
  assignmentId?: string;
  cloneTemplate?: string;
  cloneVersion?: string;
  cloneDraft?: string;
  mode?: string;
} {
  const cursors = Array.isArray(search.cursors)
    ? search.cursors
        .filter((v): v is string => typeof v === "string" && v.length <= 2048)
        .slice(-100)
    : [];
  return {
    cursors,
    pageOffset:
      cursors.length &&
      Number.isSafeInteger(Number(search.pageOffset)) &&
      Number(search.pageOffset) > 0
        ? Number(search.pageOffset)
        : 0,
    ...Object.fromEntries(
      [
        "q",
        "sort",
        "access",
        "state",
        "templateId",
        "action",
        "actor",
        "source",
        "assigned",
        "identityId",
        "runtimeId",
        "runtimeVersion",
        "publication",
        "collection",
        "assignmentId",
        "cloneTemplate",
        "cloneVersion",
        "cloneDraft",
        "mode",
      ]
        .filter(
          (key) =>
            ["string", "number", "boolean"].includes(typeof search[key]) &&
            String(search[key]).length <= 128,
        )
        .map((key) => [key, String(search[key])]),
    ),
    ...(typeof search.instanceId === "string" &&
    /^[0-9a-f-]{36}$/i.test(search.instanceId)
      ? { instanceId: search.instanceId }
      : {}),
  };
}
export function cursorWindow(cursors: string[], next: string) {
  const window = [...cursors, next];
  let removed = 0;
  while (
    window.length > 2 &&
    encodeURIComponent(JSON.stringify(window)).length > 4096
  ) {
    window.shift();
    removed++;
  }
  return { cursors: window, removed };
}
export function usePager() {
  const search = useLocation({
    select: (location) =>
      validatePage(location.search as Record<string, unknown>),
  });
  const navigate = useNavigate();
  const cursors = search.cursors ?? [];
  return {
    after: cursors.at(-1),
    search,
    filters: Object.fromEntries(
      Object.entries(search).filter(
        ([key, value]) =>
          key !== "cursors" &&
          key !== "instanceId" &&
          key !== "collection" &&
          !key.startsWith("clone") &&
          key !== "mode" &&
          typeof value === "string",
      ),
    ) as Record<string, string>,
    instanceId: search.instanceId,
    controls: (next: string | number | null | undefined) => (
      <div className="pager" aria-label="Pagination">
        <button
          disabled={!cursors.length}
          onClick={() =>
            void navigate({ to: ".", search: { ...search, cursors: [] } })
          }
        >
          First
        </button>
        <button
          disabled={
            !cursors.length ||
            ((search.pageOffset ?? 0) > 0 && cursors.length === 1)
          }
          onClick={() =>
            void navigate({
              to: ".",
              search: { ...search, cursors: cursors.slice(0, -1) },
            })
          }
        >
          Previous
        </button>
        <span>Page {(search.pageOffset ?? 0) + cursors.length + 1}</span>
        {(search.pageOffset ?? 0) > 0 && cursors.length === 1 && (
          <span>Use First to revisit earlier pages.</span>
        )}
        <button
          disabled={next == null}
          onClick={() => {
            const window = cursorWindow(cursors, String(next));
            void navigate({
              to: ".",
              search: {
                ...search,
                cursors: window.cursors,
                pageOffset: (search.pageOffset ?? 0) + window.removed,
              },
            });
          }}
        >
          Next
        </button>
      </div>
    ),
  };
}
export function ReadState({
  pending,
  error,
  children,
  stale = false,
}: {
  pending: boolean;
  error: Error | null;
  stale?: boolean;
  children: ReactNode;
}) {
  return error ? (
    <>
      <ErrorNotice error={error} />
      {stale && (
        <p role="status">
          Previously loaded information could not be refreshed. Review a fresh
          record before submitting changes.
        </p>
      )}
    </>
  ) : pending ? (
    <p role="status" className="loading">
      Connecting to the manager…
    </p>
  ) : (
    children
  );
}
export function JsonDetails({
  title,
  value,
}: {
  title: string;
  value: unknown;
}) {
  return (
    <section className="panel detail">
      <h2>{title}</h2>
      <pre>{JSON.stringify(value, null, 2)}</pre>
    </section>
  );
}

export function ListControls({
  kind,
}: {
  kind:
    | "assignments"
    | "identities"
    | "audit"
    | "runtimes"
    | "administrators"
    | "templates"
    | "drafts"
    | "operations"
    | "bindings"
    | "versions";
}) {
  const { search } = usePager();
  const navigate = useNavigate();
  const [text, setText] = useState(search.q ?? "");
  useEffect(() => setText(search.q ?? ""), [search.q]);
  useEffect(() => {
    if (text === (search.q ?? "")) return;
    const timer = setTimeout(
      () =>
        void navigate({
          to: ".",
          search: { ...search, q: text, cursors: [] },
          replace: true,
        }),
      300,
    );
    return () => clearTimeout(timer);
  }, [text, search, navigate]);
  const change = (key: string, value: string) =>
    void navigate({
      to: ".",
      search: { ...search, [key]: value || undefined, cursors: [] },
    });
  return (
    <div className="list-controls">
      <label>
        Search {kind}
        <input
          maxLength={128}
          value={text}
          onChange={(event) => setText(event.target.value)}
        />
      </label>
      <label>
        {kind === "versions" ? "Sort by version" : "Sort by ID"}
        <select
          aria-label={kind === "versions" ? "Sort by version" : "Sort by ID"}
          value={search.sort ?? "id"}
          onChange={(event) => change("sort", event.target.value)}
        >
          <option value="id">Ascending</option>
          <option value="-id">Descending</option>
        </select>
      </label>
      {(kind === "assignments" || kind === "identities") && (
        <>
          <label>
            Access
            <select
              value={search.access ?? ""}
              onChange={(event) => change("access", event.target.value)}
            >
              <option value="">All access</option>
              {["enabled", "suspended", "revoked"].map((value) => (
                <option key={value}>{value}</option>
              ))}
            </select>
          </label>
          <label>
            State
            <select
              value={search.state ?? ""}
              onChange={(event) => change("state", event.target.value)}
            >
              <option value="">All states</option>
              {[
                "Pending",
                "Provisioning",
                "Ready",
                "Blocked",
                "Suspending",
                "Suspended",
                "Retained",
                "Deleting",
                "Deleted",
                "Error",
              ].map((value) => (
                <option key={value}>{value}</option>
              ))}
            </select>
          </label>
          <label>
            Template ID
            <input
              value={search.templateId ?? ""}
              maxLength={63}
              onChange={(event) => change("templateId", event.target.value)}
            />
          </label>
        </>
      )}
      {(kind === "assignments" ||
        kind === "identities" ||
        kind === "templates") && (
        <label>
          Management source
          <select
            aria-label="Management source"
            value={search.source ?? ""}
            onChange={(e) => change("source", e.target.value)}
          >
            <option value="">All sources</option>
            <option value="manager">Manager</option>
            <option value="infrastructure">Infrastructure</option>
          </select>
        </label>
      )}
      {kind === "identities" && (
        <label>
          Assignment
          <select
            aria-label="Assignment"
            value={search.assigned ?? ""}
            onChange={(e) => change("assigned", e.target.value)}
          >
            <option value="">All users</option>
            <option value="true">Assigned</option>
            <option value="false">Unassigned</option>
          </select>
        </label>
      )}
      {kind === "drafts" && (
        <label>
          Publication state
          <select
            value={search.publication ?? ""}
            onChange={(e) => change("publication", e.target.value)}
          >
            <option value="">All drafts</option>
            <option value="draft">Unpublished</option>
            <option value="published">Published</option>
          </select>
        </label>
      )}
      {kind === "operations" && (
        <label>
          Operation state
          <select
            value={search.state ?? ""}
            onChange={(e) => change("state", e.target.value)}
          >
            <option value="">All states</option>
            {[
              "Pending",
              "Running",
              "Waiting",
              "RetryAt",
              "Succeeded",
              "Blocked",
              "Failed",
              "Superseded",
            ].map((v) => (
              <option key={v}>{v}</option>
            ))}
          </select>
        </label>
      )}
      {kind === "audit" && (
        <>
          <label>
            Exact action
            <input
              value={search.action ?? ""}
              onChange={(event) => change("action", event.target.value)}
            />
          </label>
          <label>
            Exact actor subject
            <input
              value={search.actor ?? ""}
              onChange={(event) => change("actor", event.target.value)}
            />
          </label>
        </>
      )}
    </div>
  );
}
