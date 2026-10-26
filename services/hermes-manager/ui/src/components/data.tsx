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
/** URL search keys a page may use, besides the page cursors. */
export const searchKeys = [
  "q",
  "access",
  "templateId",
  "source",
  "targetKind",
  "targetId",
  "tab",
  "version",
  "edit",
] as const;
export type PageSearch = {
  cursors?: string[];
  pageOffset?: number;
} & { [K in (typeof searchKeys)[number]]?: string };
export function validatePage(search: Record<string, unknown>): PageSearch {
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
      searchKeys
        .filter(
          (key) =>
            ["string", "number", "boolean"].includes(typeof search[key]) &&
            String(search[key]).length <= 128,
        )
        .map((key) => [key, String(search[key])]),
    ),
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
          [
            "q",
            "access",
            "templateId",
            "source",
            "targetKind",
            "targetId",
          ].includes(key) && typeof value === "string",
      ),
    ) as Record<string, string>,
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

export type Filter = {
  key: (typeof searchKeys)[number];
  label: string;
  options?: string[];
  maxLength?: number;
};
/** Search and filters kept in the URL; changing one returns to page one. */
export function ListControls({
  search: searchLabel,
  filters = [],
}: {
  search?: string;
  filters?: Filter[];
}) {
  const { search } = usePager();
  const navigate = useNavigate();
  const [text, setText] = useState(search.q ?? "");
  useEffect(() => setText(search.q ?? ""), [search.q]);
  useEffect(() => {
    if (!searchLabel || text === (search.q ?? "")) return;
    const timer = setTimeout(
      () =>
        void navigate({
          to: ".",
          search: { ...search, q: text || undefined, cursors: [] },
          replace: true,
        }),
      300,
    );
    return () => clearTimeout(timer);
  }, [text, search, navigate, searchLabel]);
  const change = (key: string, value: string) =>
    void navigate({
      to: ".",
      search: { ...search, [key]: value || undefined, cursors: [] },
    });
  return (
    <div className="list-controls">
      {searchLabel && (
        <label>
          {searchLabel}
          <input
            maxLength={128}
            value={text}
            onChange={(event) => setText(event.target.value)}
          />
        </label>
      )}
      {filters.map((filter) => (
        <label key={filter.key}>
          {filter.label}
          {filter.options ? (
            <select
              value={search[filter.key] ?? ""}
              onChange={(event) => change(filter.key, event.target.value)}
            >
              <option value="">All</option>
              {filter.options.map((value) => (
                <option key={value}>{value}</option>
              ))}
            </select>
          ) : (
            <input
              value={search[filter.key] ?? ""}
              maxLength={filter.maxLength ?? 128}
              onChange={(event) => change(filter.key, event.target.value)}
            />
          )}
        </label>
      ))}
    </div>
  );
}
