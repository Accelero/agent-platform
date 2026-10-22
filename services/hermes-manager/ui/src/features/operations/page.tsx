import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Link, useParams } from "@tanstack/react-router";
import { api } from "../../api";
import { useActorKey } from "../../app/state";
import {
  DataTable,
  ReadState,
  ListControls,
  usePager,
  JsonDetails,
  StatusBadge,
} from "../../components/data";
import { pollInterval } from "../../components/command";
export function OperationHistory({ assignmentId }: { assignmentId: string }) {
  const actor = useActorKey();
  const pager = usePager();
  const query = useQuery({
    queryKey: [actor, "history", assignmentId, pager.after, pager.filters],
    queryFn: ({ signal }) =>
      api.operationHistory(assignmentId, pager.after, signal, pager.filters),
  });
  return (
    <section>
      <h2>Operation history</h2>
      <ListControls kind="operations" />
      <ReadState
        pending={query.isPending}
        error={query.error}
        stale={query.isRefetchError}
      >
        {query.data && (
          <>
            <DataTable
              caption="Assignment operations"
              rows={query.data.items}
              empty="No operations recorded."
              columns={[
                {
                  header: "Operation",
                  cell: ({ row }) => (
                    <Link to="/operations/$id" params={{ id: row.original.id }}>
                      {row.original.kind}
                    </Link>
                  ),
                },
                { header: "Generation", accessorKey: "generation" },
                {
                  header: "Outcome",
                  cell: ({ row }) => <StatusBadge value={row.original.state} />,
                },
                { header: "Attempts", accessorKey: "attempt" },
              ]}
            />
            {pager.controls(query.data.nextCursor)}
          </>
        )}
      </ReadState>
    </section>
  );
}
export function OperationDetail() {
  const { id } = useParams({ strict: false });
  const actor = useActorKey();
  const [started] = useState(Date.now);
  const query = useQuery({
    queryKey: [actor, "operation-details", id],
    queryFn: ({ signal }) => api.operationDetails(id!, signal),
    refetchOnWindowFocus: true,
    refetchInterval: (q) =>
      pollInterval(
        started,
        Date.now(),
        q.state.dataUpdateCount,
        q.state.data?.operation.state,
      ),
  });
  return (
    <ReadState
      pending={query.isPending}
      error={query.error}
      stale={query.isRefetchError}
    >
      {query.data && (
        <>
          <h2>Operation details</h2>
          <p>
            Acknowledgement and rollout completion are separate. Verified steps
            below contain no journal inputs or credential payloads.
          </p>
          <StatusBadge value={query.data.operation.state} />
          {query.data.assignmentId && (
            <Link
              to="/assignments/$id"
              params={{ id: query.data.assignmentId }}
            >
              Inspect assignment →
            </Link>
          )}
          <JsonDetails title="Operation status" value={query.data.operation} />
          <DataTable
            caption="Journal verification"
            rows={query.data.steps}
            empty="No external steps recorded."
            columns={[
              { header: "Step", accessorKey: "key" },
              {
                header: "Recorded",
                cell: ({ row }) =>
                  new Date(row.original.recordedAt * 1000).toLocaleString(),
              },
              {
                header: "Verification",
                cell: ({ row }) =>
                  row.original.verifiedAt
                    ? new Date(row.original.verifiedAt * 1000).toLocaleString()
                    : "Unverified",
              },
            ]}
          />
        </>
      )}
    </ReadState>
  );
}
