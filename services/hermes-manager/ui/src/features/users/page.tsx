import { useQuery } from "@tanstack/react-query";
import { Link, useParams } from "@tanstack/react-router";
import { api } from "../../api";
import { useActorKey } from "../../app/state";
import {
  CopyId,
  DataTable,
  ReadState,
  usePager,
  ListControls,
  JsonDetails,
  StatusBadge,
} from "../../components/data";
import { CreateAssignment } from "../assignments/commands";
export function UsersPage() {
  const actor = useActorKey();
  const pager = usePager();
  const query = useQuery({
    queryKey: [actor, "identities", pager.after, pager.filters],
    queryFn: ({ signal }) => api.identities(pager.after, signal, pager.filters),
  });
  return (
    <>
      <CreateAssignment />
      <ListControls kind="identities" />
      <ReadState
        pending={query.isPending}
        error={query.error}
        stale={query.isRefetchError}
      >
        {query.data && (
          <>
            <DataTable
              caption="Recorded identities"
              rows={query.data.items}
              empty="No identities are recorded yet."
              columns={[
                {
                  header: "Subject",
                  cell: ({ row }) => (
                    <Link to="/users/$id" params={{ id: row.original.id }}>
                      {row.original.identity.subject}
                    </Link>
                  ),
                },
                { header: "Issuer", accessorFn: (r) => r.identity.issuer },
                {
                  header: "Directory verified",
                  cell: ({ row }) =>
                    row.original.directoryVerified
                      ? "Verified"
                      : "Not verified",
                },
                {
                  header: "Template",
                  cell: ({ row }) =>
                    row.original.assignment
                      ? `${row.original.assignment.template.id} · v${row.original.assignment.template.version}`
                      : "Unassigned",
                },
                {
                  header: "Access",
                  cell: ({ row }) =>
                    row.original.assignment ? (
                      <StatusBadge value={row.original.assignment.access} />
                    ) : (
                      "—"
                    ),
                },
                {
                  header: "Instance state",
                  cell: ({ row }) => row.original.assignment?.state ?? "—",
                },
                {
                  header: "Source",
                  cell: ({ row }) => row.original.assignment?.source ?? "—",
                },
                {
                  header: "Record ID",
                  cell: ({ row }) => <CopyId value={row.original.id} />,
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
export function UserDetail() {
  const { id } = useParams({ strict: false });
  const actor = useActorKey();
  const query = useQuery({
    queryKey: [actor, "identity", id],
    queryFn: ({ signal }) => api.identity(id!, signal),
  });
  return (
    <>
      <Link to="/users">← Users</Link>
      <ReadState
        pending={query.isPending}
        error={query.error}
        stale={query.isRefetchError}
      >
        {query.data && (
          <>
            <h2>User details</h2>
            <JsonDetails
              title="Exact identity and provenance"
              value={{
                identity: query.data.identity,
                provenance: query.data.provenance,
                directoryVerified: query.data.directoryVerified,
              }}
            />
            <CreateAssignment
              initialIdentity={query.data.identity}
              disabled={!!query.data.assignment}
            />
            {query.data.assignment ? (
              <>
                <JsonDetails
                  title="Current assignment and instance"
                  value={query.data.assignment}
                />
                <Link
                  to="/assignments/$id"
                  params={{ id: query.data.assignment.id }}
                >
                  Manage assignment and template →
                </Link>
              </>
            ) : (
              <>
                <p>This recorded identity has no assignment.</p>
              </>
            )}
          </>
        )}
      </ReadState>
    </>
  );
}
