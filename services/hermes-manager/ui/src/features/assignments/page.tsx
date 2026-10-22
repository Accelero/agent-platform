import { OperationHistory } from "../operations/page";
import { useState } from "react";
import { ApplyTemplate, BulkApply } from "./rollout";
import { ListControls } from "../../components/data";
import { CreateAssignment, AssignmentCommands } from "./commands";
import { useQuery } from "@tanstack/react-query";
import { Link, useParams } from "@tanstack/react-router";
import { api } from "../../api";
import { useActorKey } from "../../app/state";
import {
  CopyId,
  DataTable,
  JsonDetails,
  ReadState,
  StatusBadge,
  usePager,
} from "../../components/data";
export function AssignmentsPage() {
  const [selected, setSelected] = useState<Record<string, number>>({});
  const pager = usePager();
  const actor = useActorKey();
  const query = useQuery({
    queryKey: [actor, "assignments", pager.after, pager.filters],
    queryFn: ({ signal }) =>
      api.assignments(pager.after, signal, pager.filters),
  });
  return (
    <>
      <CreateAssignment />
      <BulkApply
        members={Object.entries(selected).map(
          ([assignmentId, expectedVersion]) => ({
            assignmentId,
            expectedVersion,
          }),
        )}
      />
      <>
        <ListControls kind="assignments" />
        <ReadState
          pending={query.isPending}
          error={query.error}
          stale={query.isRefetchError}
        >
          {query.data && (
            <>
              <DataTable
                caption="Instance assignments"
                rows={query.data.items}
                empty="No assignments are registered yet."
                columns={[
                  {
                    header: "Select",
                    cell: ({ row }) => (
                      <input
                        type="checkbox"
                        aria-label={`Select ${row.original.identity.subject}`}
                        checked={Object.hasOwn(selected, row.original.id)}
                        disabled={
                          !Object.hasOwn(selected, row.original.id) &&
                          Object.keys(selected).length >= 50
                        }
                        onChange={(event) => {
                          const next = { ...selected };
                          if (event.target.checked)
                            next[row.original.id] = row.original.version;
                          else delete next[row.original.id];
                          setSelected(next);
                        }}
                      />
                    ),
                  },
                  {
                    header: "Owner subject",
                    cell: ({ row }) => (
                      <>
                        <Link
                          to="/assignments/$id"
                          params={{ id: row.original.id }}
                        >
                          {row.original.identity.subject}
                        </Link>
                        <small>{row.original.identity.issuer}</small>
                      </>
                    ),
                  },
                  {
                    header: "Template",
                    cell: ({ row }) =>
                      `${row.original.template.id} · v${row.original.template.version}`,
                  },
                  {
                    header: "Access",
                    cell: ({ row }) => (
                      <StatusBadge value={row.original.access} />
                    ),
                  },
                  {
                    header: "State",
                    cell: ({ row }) => (
                      <StatusBadge value={row.original.state} />
                    ),
                  },
                  {
                    header: "Generation",
                    cell: ({ row }) =>
                      `${row.original.observedGeneration} / ${row.original.desiredGeneration}`,
                  },
                ]}
              />
              {pager.controls(query.data.nextCursor)}
            </>
          )}
        </ReadState>
      </>
    </>
  );
}
export function AssignmentDetail() {
  const { id } = useParams({ strict: false });
  const actor = useActorKey();
  const query = useQuery({
    queryKey: [actor, "assignment", id],
    queryFn: ({ signal }) => api.assignment(id!, signal),
    refetchOnWindowFocus: true,
  });
  const observation = useQuery({
    queryKey: [actor, "template-observation", id],
    queryFn: ({ signal }) => api.templateObservation(id!, signal),
    refetchOnWindowFocus: true,
  });
  const instance = useQuery({
    queryKey: [actor, "instance", query.data?.data.instanceId],
    enabled: !!query.data,
    queryFn: ({ signal }) => api.instance(query.data!.data.instanceId, signal),
    refetchOnWindowFocus: true,
  });
  const execution = useQuery({
    queryKey: [actor, "execution", id],
    queryFn: ({ signal }) => api.execution(id!, signal),
    refetchOnWindowFocus: true,
  });
  return (
    <>
      <Link to="/assignments">← Instance assignments</Link>
      <ReadState
        pending={query.isPending}
        error={query.error}
        stale={query.isRefetchError}
      >
        {query.data && (
          <>
            <h2>Assignment details</h2>
            <ApplyTemplate
              assignment={query.data.data}
              etag={query.data.etag}
            />
            <AssignmentCommands
              assignment={query.data.data}
              etag={query.data.etag}
              execution={execution.data?.data}
              instanceEtag={instance.data?.etag}
            />
            <ReadState
              pending={observation.isPending}
              error={observation.error}
              stale={observation.isRefetchError}
            >
              {observation.data && (
                <>
                  <JsonDetails
                    title="Desired and last-observed template versions"
                    value={observation.data}
                  />
                  <p>
                    An unobserved version is shown as null until native
                    reconciliation verifies its generation. Publication alone
                    never advances observed state.
                  </p>
                </>
              )}
            </ReadState>
            <OperationHistory assignmentId={id!} />
            <CopyId value={query.data.data.id} />
            <JsonDetails title="Assignment" value={query.data.data} />
            <Link
              to="/storage-bindings"
              search={{ instanceId: query.data.data.instanceId, cursors: [] }}
            >
              Inspect instance storage →
            </Link>
            <ReadState
              pending={instance.isPending}
              error={instance.error}
              stale={instance.isRefetchError}
            >
              {instance.data && (
                <JsonDetails title="Instance" value={instance.data.data} />
              )}
            </ReadState>
            <ReadState
              pending={execution.isPending}
              error={execution.error}
              stale={execution.isRefetchError}
            >
              {execution.data && (
                <section aria-label="Execution policy">
                  <p>
                    Desired policy generation{" "}
                    {execution.data.data.desiredGeneration}; applied generation{" "}
                    {execution.data.data.appliedGeneration}. Status:{" "}
                    {execution.data.data.state}.
                  </p>
                  <JsonDetails
                    title="Desired execution configuration"
                    value={execution.data.data.desired}
                  />
                  {execution.data.data.applied ? (
                    <JsonDetails
                      title="Applied execution configuration"
                      value={execution.data.data.applied}
                    />
                  ) : (
                    <p>No applied execution configuration has been verified.</p>
                  )}
                </section>
              )}
            </ReadState>
          </>
        )}
      </ReadState>
    </>
  );
}
