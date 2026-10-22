import { changedPaths } from "./model";
import { Link, useParams, useNavigate } from "@tanstack/react-router";
import { useState } from "react";
import { JsonDetails, ErrorNotice } from "../../components/data";
import { useQuery } from "@tanstack/react-query";
import { api } from "../../api";
import { useSession, useActorKey } from "../../app/state";
import {
  DataTable,
  ReadState,
  usePager,
  ListControls,
} from "../../components/data";
export function TemplatesPage() {
  const session = useSession();
  const actor = useActorKey();
  const pager = usePager();
  const navigate = useNavigate();
  const isDraft = pager.search.collection === "drafts";
  const drafts = useQuery({
    queryKey: [actor, "drafts", pager.after, pager.filters],
    enabled: isDraft,
    queryFn: ({ signal }) => api.drafts(signal, pager.after, pager.filters),
  });
  const query = useQuery({
    queryKey: [actor, "template-definitions", pager.after, pager.filters],
    enabled: !isDraft,
    queryFn: ({ signal }) =>
      api.templateDefinitions(signal, pager.after, pager.filters),
  });
  return (
    <>
      {session.data?.permittedActions.includes("template.draft.create") && (
        <Link className="primary-link" to="/templates/new">
          New template draft
        </Link>
      )}
      <div className="actions" role="group" aria-label="Template collections">
        {[false, true].map((value) => (
          <button
            key={String(value)}
            aria-pressed={value === isDraft}
            onClick={() =>
              void navigate({
                to: "/templates",
                search: {
                  collection: value ? "drafts" : undefined,
                  cursors: [],
                },
              })
            }
          >
            {value ? "Drafts" : "Published templates"}
          </button>
        ))}
      </div>
      <ListControls kind={isDraft ? "drafts" : "templates"} />
      {isDraft ? (
        <ReadState
          pending={drafts.isPending}
          error={drafts.error}
          stale={drafts.isRefetchError}
        >
          {drafts.data && (
            <>
              <DataTable
                caption="Manager-owned drafts"
                rows={drafts.data.items}
                empty="No drafts match this view."
                columns={[
                  {
                    header: "Template",
                    cell: ({ row }) => (
                      <Link
                        to="/template-drafts/$id"
                        params={{ id: row.original.id }}
                      >
                        {row.original.templateId}
                      </Link>
                    ),
                  },
                  { header: "Draft revision", accessorKey: "version" },
                  {
                    header: "Publication",
                    cell: ({ row }) =>
                      row.original.publishedVersion
                        ? `Published v${row.original.publishedVersion}`
                        : "Unpublished",
                  },
                  {
                    header: "Actions",
                    cell: ({ row }) => (
                      <Link
                        to="/templates/new"
                        search={{ cloneDraft: row.original.id }}
                      >
                        Clone draft
                      </Link>
                    ),
                  },
                ]}
              />
              {pager.controls(drafts.data.nextCursor)}
            </>
          )}
        </ReadState>
      ) : (
        <ReadState
          pending={query.isPending}
          error={query.error}
          stale={query.isRefetchError}
        >
          {query.data && (
            <>
              <DataTable
                caption="Hermes template definitions"
                rows={query.data.items}
                empty="No templates match this view."
                columns={[
                  {
                    header: "Template",
                    cell: ({ row }) => (
                      <Link
                        to="/templates/$id"
                        params={{ id: row.original.id }}
                      >
                        {row.original.id}
                      </Link>
                    ),
                  },
                  {
                    header: "Version",
                    cell: ({ row }) =>
                      row.original.versions[0]?.version ?? "Unpublished",
                  },
                  {
                    header: "Image digest",
                    cell: ({ row }) =>
                      row.original.definition?.image ??
                      "Definition unavailable",
                  },
                  { header: "Source", accessorKey: "source" },
                  { header: "Publication", accessorKey: "publicationState" },
                  {
                    header: "Condition",
                    cell: ({ row }) => row.original.condition ?? "Valid",
                  },
                  {
                    header: "Assigned users",
                    accessorKey: "assignedUserCount",
                  },
                  {
                    header: "Actions",
                    cell: ({ row }) =>
                      row.original.versions.length > 0 &&
                      row.original.definition ? (
                        <Link
                          to="/templates/new"
                          search={{
                            cloneTemplate: row.original.id,
                            cloneVersion: String(
                              row.original.versions[0].version,
                            ),
                          }}
                        >
                          Clone template
                        </Link>
                      ) : (
                        "Inspect definition"
                      ),
                  },
                ]}
              />
              {pager.controls(query.data.nextCursor)}
            </>
          )}
        </ReadState>
      )}
    </>
  );
}

export function TemplateDetail() {
  const { id } = useParams({ strict: false });
  const actor = useActorKey();
  const [tab, setTab] = useState("Summary");
  const query = useQuery({
    queryKey: [actor, "template-definition", id],
    queryFn: ({ signal }) => api.templateDefinition(id!, signal),
  });
  return (
    <>
      <Link to="/templates">← Template definitions</Link>
      <ReadState
        pending={query.isPending}
        error={query.error}
        stale={query.isRefetchError}
      >
        {query.data && (
          <>
            <h2>{id}</h2>
            <p>
              {query.data.source === "infrastructure"
                ? "Infrastructure-owned definition. Read only in the dashboard."
                : "Approved manager catalog definition."}
            </p>
            <div
              className="actions"
              role="group"
              aria-label="Template sections"
            >
              {[
                "Summary",
                "Deployment",
                "Managed configuration",
                "Versions",
                "Assigned users",
              ].map((name) => (
                <button
                  key={name}
                  aria-pressed={name === tab}
                  onClick={() => setTab(name)}
                >
                  {name}
                </button>
              ))}
            </div>
            {tab === "Summary" && (
              <JsonDetails
                title="Template summary"
                value={{
                  source: query.data.source,
                  publicationState: query.data.publicationState,
                  schemaCoverage: query.data.schemaCoverage,
                  condition: query.data.condition,
                  rejectedDefinition: query.data.rejectedDefinition,
                  assignedUserCount: query.data.assignedUserCount,
                }}
              />
            )}
            {tab === "Deployment" && (
              <JsonDetails
                title="Deployment settings"
                value={
                  query.data.definition
                    ? Object.fromEntries(
                        Object.entries(query.data.definition).filter(
                          ([key]) => key !== "adminConfig",
                        ),
                      )
                    : null
                }
              />
            )}
            {tab === "Managed configuration" && (
              <>
                <p>
                  Managed overrides apply over retained user settings. Mandatory
                  platform settings enforce managed execution, authentication
                  and credential delivery independently. Removing an override
                  exposes a retained user value; it does not erase user
                  configuration.
                </p>
                <p>
                  Preview includes managed values only. Private native user
                  configuration is never read.
                </p>
                <JsonDetails
                  title="Managed overrides"
                  value={query.data.definition?.adminConfig ?? {}}
                />
                {query.data.redactedPaths.length > 0 && (
                  <p>Sensitive configuration paths were redacted.</p>
                )}
              </>
            )}
            {tab === "Versions" && (
              <VersionInspector
                id={id!}
                versions={query.data.versions}
                source={query.data.source}
              />
            )}
            {tab === "Assigned users" && (
              <Link to="/assignments" search={{ templateId: id, cursors: [] }}>
                Inspect {query.data.assignedUserCount} assigned users →
              </Link>
            )}
          </>
        )}
      </ReadState>
    </>
  );
}

function VersionInspector({
  id,
  versions,
  source,
}: {
  id: string;
  versions: { version: number; digest: string }[];
  source: string;
}) {
  const actor = useActorKey();
  const pager = usePager();
  const catalog = useQuery({
    queryKey: [
      actor,
      "template-version-page",
      id,
      pager.after,
      pager.filters.q,
      pager.filters.sort,
    ],
    queryFn: ({ signal }) =>
      api.templates(signal, pager.after, {
        templateId: id,
        q: pager.filters.q ?? "",
        sort: pager.filters.sort ?? "id",
      }),
  });
  const choices =
    catalog.data?.items.map((v) => v.version) ??
    versions.slice(0, 50).map((v) => v.version);
  const [version, setVersion] = useState(versions[0]?.version);
  const [compare, setCompare] = useState<number>();
  const current = useQuery({
    queryKey: [actor, "template-version", id, version],
    enabled: !!version,
    queryFn: ({ signal }) => api.templateVersion(id, version!, signal),
  });
  const previous = useQuery({
    queryKey: [actor, "template-version", id, compare],
    enabled: !!compare,
    queryFn: ({ signal }) => api.templateVersion(id, compare!, signal),
  });
  return (
    <>
      <ListControls kind="versions" />
      {catalog.error && <ErrorNotice error={catalog.error} />}
      <label>
        Inspect published version
        <select
          value={version ?? ""}
          onChange={(e) => setVersion(Number(e.target.value))}
        >
          {version && !choices.includes(version) && (
            <option value={version}>Selected v{version}</option>
          )}
          {choices.map((v) => (
            <option key={v} value={v}>
              v{v}
            </option>
          ))}
        </select>
      </label>
      <label>
        Compare with version
        <select
          value={compare ?? ""}
          onChange={(e) =>
            setCompare(e.target.value ? Number(e.target.value) : undefined)
          }
        >
          <option value="">No comparison</option>
          {compare && !choices.includes(compare) && (
            <option value={compare}>Selected v{compare}</option>
          )}
          {choices.map((v) => (
            <option key={v} value={v}>
              v{v}
            </option>
          ))}
        </select>
      </label>
      {version && (
        <>
          <Link
            to="/templates/new"
            search={{ cloneTemplate: id, cloneVersion: String(version) }}
          >
            Clone selected version
          </Link>
          {source === "manager" && (
            <Link
              to="/templates/new"
              search={{
                cloneTemplate: id,
                cloneVersion: String(version),
                mode: "revision",
              }}
            >
              Create next version draft
            </Link>
          )}
        </>
      )}
      <ReadState
        pending={current.isPending && !!version}
        error={current.error}
        stale={current.isRefetchError}
      >
        {current.data && (
          <>
            <JsonDetails
              title="Immutable version and schema inputs"
              value={current.data}
            />
            {previous.data && (
              <JsonDetails
                title="Semantic changed paths"
                value={changedPaths(
                  previous.data.definition,
                  current.data.definition,
                )}
              />
            )}
          </>
        )}
      </ReadState>
      {catalog.data && pager.controls(catalog.data.nextCursor)}
      {previous.error && <ErrorNotice error={previous.error} />}
    </>
  );
}
