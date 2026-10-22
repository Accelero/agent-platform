import { ListControls } from "../../components/data";
import { useQuery } from "@tanstack/react-query";
import { Link, useParams } from "@tanstack/react-router";
import { api } from "../../api";
import { useActorKey } from "../../app/state";
import {
  DataTable,
  ReadState,
  usePager,
  JsonDetails,
  StatusBadge,
} from "../../components/data";
export function ActivityPage() {
  const actor = useActorKey();
  const pager = usePager();
  const query = useQuery({
    queryKey: [actor, "audit", pager.after, pager.filters],
    queryFn: ({ signal }) => api.audit(pager.after, signal, pager.filters),
  });
  return (
    <>
      <ListControls kind="audit" />
      <ReadState
        pending={query.isPending}
        error={query.error}
        stale={query.isRefetchError}
      >
        {query.data && (
          <>
            <DataTable
              caption="Audit events"
              rows={query.data.items}
              empty="No administrator changes have been recorded yet."
              columns={[
                {
                  header: "Action",
                  cell: ({ row }) => (
                    <Link to="/activity/$id" params={{ id: row.original.id }}>
                      {row.original.action}
                    </Link>
                  ),
                },
                {
                  header: "Outcome",
                  cell: ({ row }) => (
                    <StatusBadge value={row.original.outcome ?? "Unknown"} />
                  ),
                },
                {
                  header: "Operation",
                  cell: ({ row }) =>
                    row.original.operationId ? (
                      <Link
                        to="/operations/$id"
                        params={{ id: row.original.operationId }}
                      >
                        Inspect operation
                      </Link>
                    ) : (
                      "Configuration recorded"
                    ),
                },
                {
                  header: "Administrator",
                  cell: ({ row }) => (
                    <>
                      {row.original.actor.subject}
                      <small>{row.original.actor.issuer}</small>
                    </>
                  ),
                },
                {
                  header: "Assignment",
                  cell: ({ row }) =>
                    row.original.assignmentId ? (
                      <Link
                        to="/assignments/$id"
                        params={{ id: row.original.assignmentId }}
                      >
                        {row.original.assignmentId}
                      </Link>
                    ) : (
                      <span>
                        {row.original.entityType}: {row.original.entityId}
                      </span>
                    ),
                },
                {
                  header: "Time",
                  cell: ({ row }) =>
                    new Date(row.original.occurredAt * 1000).toLocaleString(),
                },
              ]}
            />
            {pager.controls(query.data.nextCursor)}
          </>
        )}
      </ReadState>
    </>
  );
}

export function ActivityDetail() {
  const { id } = useParams({ strict: false });
  const actor = useActorKey();
  const query = useQuery({
    queryKey: [actor, "audit-detail", id],
    queryFn: ({ signal }) => api.auditDetails(id!, signal),
  });
  return (
    <>
      <Link to="/activity">← Activity</Link>
      <ReadState
        pending={query.isPending}
        error={query.error}
        stale={query.isRefetchError}
      >
        {query.data && (
          <>
            <h2>Change details</h2>
            <JsonDetails
              title="Actor, affected record and outcome"
              value={query.data.event}
            />
            <JsonDetails title="Redacted change" value={query.data.change} />
            <p>
              Only lifecycle and approved reference metadata are shown. Private
              values and journal payloads are omitted.
            </p>
            {query.data.event.operationId && (
              <>
                <Link
                  to="/operations/$id"
                  params={{ id: query.data.event.operationId }}
                >
                  Inspect operation →
                </Link>
              </>
            )}
            {query.data.event.assignmentId && (
              <Link
                to="/assignments/$id"
                params={{ id: query.data.event.assignmentId }}
              >
                Inspect assignment →
              </Link>
            )}
            {query.data.event.entityType === "runtime" &&
              query.data.event.entityId && (
                <Link
                  to="/terminal-runtimes/$id"
                  params={{ id: query.data.event.entityId }}
                >
                  Inspect runtime →
                </Link>
              )}
            {query.data.event.entityType === "storage_binding" &&
              query.data.event.entityId && (
                <Link
                  to="/storage-bindings/$id"
                  params={{ id: query.data.event.entityId }}
                >
                  Inspect storage binding →
                </Link>
              )}
            {query.data.event.entityType === "template" &&
              query.data.event.entityId && (
                <Link
                  to="/templates/$id"
                  params={{ id: query.data.event.entityId }}
                >
                  Inspect template →
                </Link>
              )}
            {query.data.event.entityType === "administrator" &&
              query.data.event.entityId && (
                <Link
                  to="/administrators/$id"
                  params={{ id: query.data.event.entityId }}
                >
                  Inspect administrator →
                </Link>
              )}
          </>
        )}
      </ReadState>
    </>
  );
}
