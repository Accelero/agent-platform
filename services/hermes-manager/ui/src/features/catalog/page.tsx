import { lazy, Suspense, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Link, useNavigate, useParams } from "@tanstack/react-router";
import { api, type CatalogCollection } from "../../api";
import { useActorKey } from "../../app/state";
import { Command, usePermitted } from "../../components/command";
import {
  DataTable,
  JsonDetails,
  ListControls,
  ReadState,
  StatusBadge,
  usePager,
} from "../../components/data";
import { initialDefinition } from "./initial";

const DraftEditor = lazy(() => import("./editor"));
const nouns: Record<CatalogCollection, [string, string]> = {
  templates: ["Hermes template", "Hermes templates"],
  "terminal-backends": ["terminal backend", "Terminal backends"],
};
const detailRoute = {
  templates: "/templates/$id",
  "terminal-backends": "/terminal-backends/$id",
} as const;

export function CatalogPage({ collection }: { collection: CatalogCollection }) {
  const actor = useActorKey();
  const pager = usePager();
  const navigate = useNavigate();
  const permitted = usePermitted("administer");
  const [newId, setNewId] = useState("");
  const [singular, plural] = nouns[collection];
  const query = useQuery({
    queryKey: [actor, collection, pager.after, pager.filters.q],
    queryFn: ({ signal }) =>
      api.catalog(collection, pager.after, signal, pager.filters.q),
  });
  const validId = /^[a-z]([-a-z0-9]{0,61}[a-z0-9])?$/.test(newId);
  return (
    <>
      <p>
        Each version is a draft until published. Published versions are
        immutable and can be selected; withdrawn ones stay pinned where used but
        cannot be newly selected.
      </p>
      <ListControls search={`Search ${plural.toLowerCase()}`} />
      <ReadState
        pending={query.isPending}
        error={query.error}
        stale={query.isRefetchError}
      >
        {query.data && (
          <>
            <DataTable
              caption={plural}
              rows={query.data.items}
              empty={`No ${plural.toLowerCase()} match.`}
              columns={[
                {
                  header: "ID",
                  cell: ({ row }) => (
                    <Link
                      to={detailRoute[collection]}
                      params={{ id: row.original.id }}
                    >
                      {row.original.id}
                    </Link>
                  ),
                },
                { header: "Versions", accessorKey: "versions" },
                {
                  header: "Latest published",
                  cell: ({ row }) =>
                    row.original.latestPublished
                      ? `v${row.original.latestPublished}`
                      : "None",
                },
                {
                  header: "Draft",
                  cell: ({ row }) =>
                    row.original.draft ? `v${row.original.draft}` : "—",
                },
                { header: "Assignments", accessorKey: "assignments" },
              ]}
            />
            {pager.controls(query.data.nextCursor)}
          </>
        )}
      </ReadState>
      {permitted && (
        <section className="panel detail">
          <h2>New {singular}</h2>
          <form
            onSubmit={(event) => {
              event.preventDefault();
              void navigate({
                to: detailRoute[collection],
                params: { id: newId },
                search: { edit: "new" },
              });
            }}
          >
            <label>
              ID
              <input
                value={newId}
                maxLength={63}
                pattern="[a-z]([\-a-z0-9]*[a-z0-9])?"
                onChange={(event) => setNewId(event.target.value)}
              />
            </label>
            <button disabled={!validId}>Start draft</button>
          </form>
        </section>
      )}
    </>
  );
}

export function CatalogDetail({
  collection,
}: {
  collection: CatalogCollection;
}) {
  const { id } = useParams({ strict: false }) as { id: string };
  const actor = useActorKey();
  const pager = usePager();
  const navigate = useNavigate();
  const [singular] = nouns[collection];
  const versions = useQuery({
    queryKey: [actor, collection, id, "versions", pager.after],
    queryFn: ({ signal }) => api.versions(collection, id, pager.after, signal),
  });
  const newest = versions.data?.items.reduce(
    (max, item) => Math.max(max, item.version),
    0,
  );
  const selected = Number(pager.search.version) || newest || 0;
  const version = useQuery({
    queryKey: [actor, collection, id, "version", selected],
    enabled: selected > 0,
    queryFn: ({ signal }) => api.version(collection, id, selected, signal),
  });
  const edit = pager.search.edit;
  const select = (next: { version?: string; edit?: string }) =>
    void navigate({ to: ".", search: { ...pager.search, ...next } });
  const path = `/api/v1/${collection}/${encodeURIComponent(id)}/versions/${selected}`;
  const record = version.data?.data;
  const fresh = versions.data?.items.length === 0;
  const editor =
    (edit === "new" && (fresh || record)) ||
    (record && edit === String(record.version)) ? (
      <Suspense fallback={<p>Loading editor…</p>}>
        <DraftEditor
          collection={collection}
          id={id}
          version={edit === "new" ? undefined : record!.version}
          etag={edit === "new" ? undefined : version.data!.etag}
          base={record?.definition ?? initialDefinition(collection)}
          onSaved={(receipt) =>
            select({ version: String(receipt.version), edit: undefined })
          }
        />
      </Suspense>
    ) : null;
  return (
    <>
      <ReadState
        pending={versions.isPending}
        error={versions.error}
        stale={versions.isRefetchError}
      >
        {versions.data && (
          <>
            <DataTable
              caption={`Versions of ${id}`}
              rows={versions.data.items}
              empty={`No versions yet. Save the first draft of this ${singular}.`}
              columns={[
                {
                  header: "Version",
                  cell: ({ row }) => (
                    <button
                      className="link"
                      aria-current={
                        row.original.version === selected ? "true" : undefined
                      }
                      onClick={() =>
                        select({
                          version: String(row.original.version),
                          edit: undefined,
                        })
                      }
                    >
                      v{row.original.version}
                    </button>
                  ),
                },
                {
                  header: "State",
                  cell: ({ row }) => <StatusBadge value={row.original.state} />,
                },
                { header: "Revision", accessorKey: "revision" },
                { header: "Updated", accessorKey: "updatedAt" },
                { header: "Assignments", accessorKey: "assignments" },
              ]}
            />
            {pager.controls(versions.data.nextCursor)}
          </>
        )}
      </ReadState>
      {editor}
      {!editor && selected > 0 && (
        <ReadState
          pending={version.isPending}
          error={version.error}
          stale={version.isRefetchError}
        >
          {record && (
            <>
              <div className="actions">
                {record.state === "draft" && (
                  <>
                    <PermittedButton
                      label="Edit draft"
                      onClick={() => select({ edit: String(record.version) })}
                    />
                    <Command
                      title={`Publish v${record.version}`}
                      permission="administer"
                      method="POST"
                      path={`${path}/publish`}
                      etag={version.data!.etag}
                      current={record}
                      bodyless
                      warning="Publishing validates this draft against the installation and freezes it. Existing assignments keep their versions."
                    />
                  </>
                )}
                {record.state === "published" && (
                  <Command
                    title={`Withdraw v${record.version}`}
                    permission="administer"
                    method="POST"
                    path={`${path}/withdraw`}
                    etag={version.data!.etag}
                    current={record}
                    bodyless
                    warning="Withdrawn versions cannot be newly selected. Assignments pinned to it keep running it."
                  />
                )}
                <PermittedButton
                  label="Start next version from this one"
                  onClick={() => select({ edit: "new" })}
                />
              </div>
              <JsonDetails
                title={`${id} v${record.version} (${record.state})`}
                value={record.definition}
              />
            </>
          )}
        </ReadState>
      )}
    </>
  );
}

function PermittedButton({
  label,
  onClick,
}: {
  label: string;
  onClick: () => void;
}) {
  return usePermitted("administer") ? (
    <button onClick={onClick}>{label}</button>
  ) : null;
}
