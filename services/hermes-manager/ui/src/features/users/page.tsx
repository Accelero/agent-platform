import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Link, useNavigate, useParams } from "@tanstack/react-router";
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
  const navigate = useNavigate();
  const directory = pager.search.collection === "directory";
  const query = useQuery({
    queryKey: [actor, "identities", pager.after, pager.filters],
    enabled: !directory,
    queryFn: ({ signal }) => api.identities(pager.after, signal, pager.filters),
  });
  return (
    <>
      <div className="actions">
        {[false, true].map((value) => (
          <button
            key={String(value)}
            aria-pressed={directory === value}
            onClick={() =>
              void navigate({
                to: "/users",
                search: {
                  collection: value ? "directory" : "recorded",
                  cursors: [],
                },
              })
            }
          >
            {value ? "Keycloak directory" : "Recorded identities"}
          </button>
        ))}
      </div>
      {directory ? (
        <DirectoryUsers />
      ) : (
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
      )}
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

function DirectoryUsers() {
  const actor = useActorKey();
  const pager = usePager();
  const navigate = useNavigate();
  const [selected, setSelected] = useState<{
    issuer: string;
    subject: string;
  }>();
  const query = useQuery({
    queryKey: [actor, "directory-users", pager.after, pager.search.q],
    queryFn: ({ signal }) =>
      api.directoryUsers(pager.after, signal, pager.search.q),
  });
  return (
    <section>
      <p>
        Directory results are verified provider records. Names and email are
        labels; assignments use the exact issuer and subject. Directory lookup
        does not grant administration access.
      </p>
      <label>
        Search Keycloak users
        <input
          type="search"
          value={pager.search.q ?? ""}
          maxLength={128}
          onChange={(e) => {
            setSelected(undefined);
            void navigate({
              to: "/users",
              search: {
                collection: "directory",
                q: e.target.value,
                cursors: [],
              },
            });
          }}
        />
      </label>
      <ReadState
        pending={query.isPending}
        error={query.error}
        stale={query.isRefetchError}
      >
        {query.data &&
          (!query.data.enabled ? (
            <p>
              Keycloak directory search is not configured. Recorded identities
              remain available.
            </p>
          ) : (
            <>
              <DataTable
                caption="Keycloak directory users"
                rows={query.data.items}
                empty="No directory users match this search."
                columns={[
                  {
                    header: "Identity",
                    cell: ({ row }) => (
                      <>
                        <strong>{row.original.identity.subject}</strong>
                        <small>{row.original.identity.issuer}</small>
                      </>
                    ),
                  },
                  { header: "Username", accessorKey: "username" },
                  { header: "Display name", accessorKey: "displayName" },
                  { header: "Email", accessorKey: "email" },
                  {
                    header: "Provider status",
                    cell: ({ row }) =>
                      row.original.enabled ? "Enabled" : "Disabled",
                  },
                  {
                    header: "Assignment",
                    cell: ({ row }) => (
                      <button
                        disabled={!row.original.enabled}
                        onClick={() => setSelected(row.original.identity)}
                      >
                        Select exact identity
                      </button>
                    ),
                  },
                ]}
              />
              {pager.controls(query.data.nextCursor)}
              {selected && (
                <section key={JSON.stringify(selected)}>
                  <JsonDetails
                    title="Selected directory identity"
                    value={selected}
                  />
                  <CreateAssignment initialIdentity={selected} />
                </section>
              )}
            </>
          ))}
      </ReadState>
    </section>
  );
}
