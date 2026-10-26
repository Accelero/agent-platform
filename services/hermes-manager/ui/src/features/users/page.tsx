import { useQuery } from "@tanstack/react-query";
import { Link, useNavigate, useParams } from "@tanstack/react-router";
import { api } from "../../api";
import { useActorKey } from "../../app/state";
import {
  CopyId,
  DataTable,
  ListControls,
  ReadState,
  StatusBadge,
  usePager,
} from "../../components/data";
import { CreateAssignment } from "../assignments/editor";

export function UsersPage() {
  const { search } = usePager();
  const navigate = useNavigate();
  const tab = search.tab === "directory" ? "directory" : "known";
  return (
    <>
      <div className="tabs" role="tablist" aria-label="User sources">
        {(
          [
            ["known", "Known identities"],
            ["directory", "Keycloak directory"],
          ] as const
        ).map(([value, label]) => (
          <button
            key={value}
            role="tab"
            aria-selected={tab === value}
            onClick={() =>
              void navigate({
                to: ".",
                search: {
                  tab: value === "known" ? undefined : value,
                  cursors: [],
                },
              })
            }
          >
            {label}
          </button>
        ))}
      </div>
      {tab === "known" ? <KnownIdentities /> : <Directory />}
    </>
  );
}

function KnownIdentities() {
  const actor = useActorKey();
  const pager = usePager();
  const query = useQuery({
    queryKey: [actor, "identities", pager.after, pager.filters.q],
    queryFn: ({ signal }) =>
      api.identities(pager.after, signal, pager.filters.q),
  });
  return (
    <>
      <p>
        Identities the manager has recorded: everyone with an assignment, and
        everyone who signed in. Email and names are labels, never keys.
      </p>
      <ListControls search="Search identities" />
      <ReadState
        pending={query.isPending}
        error={query.error}
        stale={query.isRefetchError}
      >
        {query.data && (
          <>
            <DataTable
              caption="Known identities"
              rows={query.data.items}
              empty="No identities match."
              columns={[
                {
                  header: "Identity",
                  cell: ({ row }) => (
                    <Link to="/users/$id" params={{ id: row.original.id }}>
                      {row.original.identity.subject}
                      <small>{row.original.identity.issuer}</small>
                    </Link>
                  ),
                },
                {
                  header: "Label",
                  cell: ({ row }) =>
                    row.original.displayName ?? row.original.email ?? "—",
                },
                {
                  header: "Signed in",
                  cell: ({ row }) =>
                    row.original.verified ? "Yes" : "Not yet",
                },
                {
                  header: "Assignment",
                  cell: ({ row }) =>
                    row.original.assignmentId ? (
                      <Link
                        to="/assignments/$id"
                        params={{ id: row.original.assignmentId }}
                      >
                        Open
                      </Link>
                    ) : (
                      "None"
                    ),
                },
                {
                  header: "Administrator",
                  cell: ({ row }) =>
                    row.original.administrator ? "Granted" : "—",
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

function Directory() {
  const actor = useActorKey();
  const pager = usePager();
  const query = useQuery({
    queryKey: [actor, "directory", pager.after, pager.filters.q],
    queryFn: ({ signal }) =>
      api.directoryUsers(pager.after, signal, pager.filters.q),
  });
  return (
    <>
      <p>
        A read-only lookup in Keycloak. Finding someone here grants nothing; an
        assignment or grant must still be created explicitly.
      </p>
      <ListControls search="Search the directory" />
      <ReadState
        pending={query.isPending}
        error={query.error}
        stale={query.isRefetchError}
      >
        {query.data &&
          (query.data.enabled ? (
            <>
              <DataTable
                caption="Directory users"
                rows={query.data.items}
                empty="No directory users match."
                columns={[
                  {
                    header: "Identity",
                    cell: ({ row }) => (
                      <>
                        {row.original.identity.subject}
                        <small>{row.original.identity.issuer}</small>
                      </>
                    ),
                  },
                  {
                    header: "Username",
                    cell: ({ row }) => row.original.username ?? "—",
                  },
                  {
                    header: "Name",
                    cell: ({ row }) => row.original.displayName ?? "—",
                  },
                  {
                    header: "Email",
                    cell: ({ row }) => row.original.email ?? "—",
                  },
                  {
                    header: "Status",
                    cell: ({ row }) => (
                      <StatusBadge
                        value={row.original.enabled ? "enabled" : "disabled"}
                      />
                    ),
                  },
                  {
                    header: "Action",
                    cell: ({ row }) =>
                      row.original.enabled && (
                        <CreateAssignment identity={row.original.identity} />
                      ),
                  },
                ]}
              />
              {pager.controls(query.data.nextCursor)}
            </>
          ) : (
            <p role="status">
              The Keycloak directory is not configured for this installation.
            </p>
          ))}
      </ReadState>
    </>
  );
}

export function UserDetail() {
  const { id } = useParams({ from: "/users/$id" });
  const actor = useActorKey();
  const query = useQuery({
    queryKey: [actor, "identity", id],
    queryFn: ({ signal }) => api.identity(id, signal),
  });
  return (
    <ReadState
      pending={query.isPending}
      error={query.error}
      stale={query.isRefetchError}
    >
      {query.data && (
        <section className="panel detail">
          <h2>{query.data.identity.subject}</h2>
          <dl>
            <dt>Issuer</dt>
            <dd>
              <code>{query.data.identity.issuer}</code>
            </dd>
            <dt>Subject</dt>
            <dd>
              <CopyId value={query.data.identity.subject} />
            </dd>
            <dt>Labels</dt>
            <dd>
              {[query.data.displayName, query.data.email]
                .filter(Boolean)
                .join(" · ") || "None"}
            </dd>
            <dt>Signed in to the manager</dt>
            <dd>{query.data.verified ? "Yes" : "Not yet"}</dd>
            <dt>Administrator</dt>
            <dd>{query.data.administrator ? "Granted" : "No grant"}</dd>
          </dl>
          <div className="actions">
            {query.data.assignmentId ? (
              <Link
                to="/assignments/$id"
                params={{ id: query.data.assignmentId }}
              >
                Open assignment
              </Link>
            ) : (
              <CreateAssignment identity={query.data.identity} />
            )}
          </div>
        </section>
      )}
    </ReadState>
  );
}
