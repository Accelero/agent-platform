import { useQuery } from "@tanstack/react-query";
import { Link, useParams } from "@tanstack/react-router";
import { api, type Schemas } from "../../api";
import { useActorKey } from "../../app/state";
import {
  DataTable,
  JsonDetails,
  ListControls,
  ReadState,
  usePager,
} from "../../components/data";

const targetKinds = [
  "assignment",
  "instance",
  "template",
  "terminal_backend",
  "secret",
  "administrator",
];
function Actor({ actor }: { actor: Schemas["AuditActor"] }) {
  return actor.identity ? (
    <>
      {actor.identity.subject}
      <small>{actor.identity.issuer}</small>
    </>
  ) : (
    <>{actor.kind}</>
  );
}
function Target({ target }: { target: Schemas["AuditEvent"]["target"] }) {
  switch (target.kind) {
    case "assignment":
      return (
        <Link to="/assignments/$id" params={{ id: target.id }}>
          assignment {target.id}
        </Link>
      );
    case "template":
      return (
        <Link to="/templates/$id" params={{ id: target.id }}>
          template {target.id}
        </Link>
      );
    case "terminal_backend":
      return (
        <Link to="/terminal-backends/$id" params={{ id: target.id }}>
          terminal backend {target.id}
        </Link>
      );
    case "administrator":
      return (
        <Link to="/administrators/$id" params={{ id: target.id }}>
          administrator {target.id}
        </Link>
      );
    default:
      return (
        <>
          {target.kind} <code>{target.id}</code>
        </>
      );
  }
}
export function ActivityPage() {
  const actor = useActorKey();
  const pager = usePager();
  const query = useQuery({
    queryKey: [actor, "audit", pager.after, pager.filters],
    queryFn: ({ signal }) =>
      api.audit(pager.after, signal, {
        targetKind: pager.filters.targetKind,
        targetId: pager.filters.targetId,
      }),
  });
  return (
    <>
      <p>
        A recorded change was accepted; the instance phase shows whether the
        controller has applied it. Change details are redacted and never hold
        secret values.
      </p>
      <ListControls
        filters={[
          { key: "targetKind", label: "Record kind", options: targetKinds },
          { key: "targetId", label: "Exact record ID" },
        ]}
      />
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
              empty="No changes match."
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
                  header: "Record",
                  cell: ({ row }) => <Target target={row.original.target} />,
                },
                {
                  header: "Actor",
                  cell: ({ row }) => <Actor actor={row.original.actor} />,
                },
                { header: "Time", accessorKey: "occurredAt" },
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
  const { id } = useParams({ from: "/activity/$id" });
  const actor = useActorKey();
  const query = useQuery({
    queryKey: [actor, "audit-event", id],
    queryFn: ({ signal }) => api.auditEvent(id, signal),
  });
  return (
    <ReadState
      pending={query.isPending}
      error={query.error}
      stale={query.isRefetchError}
    >
      {query.data && (
        <>
          <section className="panel detail">
            <h2>{query.data.action}</h2>
            <dl>
              <dt>Record</dt>
              <dd>
                <Target target={query.data.target} />
              </dd>
              <dt>Actor</dt>
              <dd>
                <Actor actor={query.data.actor} />
              </dd>
              <dt>Time</dt>
              <dd>{query.data.occurredAt}</dd>
              <dt>Request</dt>
              <dd>
                <code>{query.data.requestId}</code>
              </dd>
            </dl>
          </section>
          <JsonDetails title="Recorded change" value={query.data.change} />
        </>
      )}
    </ReadState>
  );
}
