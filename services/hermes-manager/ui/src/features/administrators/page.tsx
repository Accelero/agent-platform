import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Link, useParams } from "@tanstack/react-router";
import { api } from "../../api";
import { useActorKey } from "../../app/state";
import { Command, usePermitted } from "../../components/command";
import { DataTable, ReadState, usePager } from "../../components/data";

export function AdministratorsPage() {
  const actor = useActorKey();
  const pager = usePager();
  const manager = usePermitted("administrator.manage");
  const [candidateSearch, setCandidateSearch] = useState("");
  const [candidateAfter, setCandidateAfter] = useState<string>();
  const [chosen, setChosen] = useState("");
  const query = useQuery({
    queryKey: [actor, "administrators", pager.after],
    queryFn: ({ signal }) => api.administrators(pager.after, signal),
  });
  const candidates = useQuery({
    queryKey: [
      actor,
      "administrator-candidates",
      candidateSearch,
      candidateAfter,
    ],
    enabled: manager,
    queryFn: ({ signal }) =>
      api.administratorCandidates(candidateAfter, signal, candidateSearch),
  });
  const selected = candidates.data?.items.find((item) => item.id === chosen);
  return (
    <>
      <p>
        Keycloak authenticates exact identities; active PostgreSQL grants are
        the only source of administrator permission. Token roles grant nothing.
        Revocation denies the next request, including assets and bearer tokens.
      </p>
      <ReadState
        pending={query.isPending}
        error={query.error}
        stale={query.isRefetchError}
      >
        {query.data && (
          <>
            <DataTable
              caption="Administrator grants"
              rows={query.data.items}
              empty="No administrator grants."
              columns={[
                {
                  header: "Identity",
                  cell: ({ row }) => (
                    <Link
                      to="/administrators/$id"
                      params={{ id: row.original.identityId }}
                    >
                      {row.original.identity.subject}
                      <small>{row.original.identity.issuer}</small>
                    </Link>
                  ),
                },
                {
                  header: "Active",
                  cell: ({ row }) =>
                    row.original.active ? "Active" : "Revoked",
                },
                {
                  header: "Manage administrators",
                  cell: ({ row }) =>
                    row.original.canManageAdministrators
                      ? "Permitted"
                      : "Not permitted",
                },
                { header: "Updated", accessorKey: "updatedAt" },
              ]}
            />
            {pager.controls(query.data.nextCursor)}
          </>
        )}
      </ReadState>
      {manager && (
        <section className="panel detail">
          <h2>Add an administrator</h2>
          <p>
            Only identities that signed in to the manager at least once can be
            granted. Signing in alone grants nothing.
          </p>
          <label>
            Search signed-in identities
            <input
              type="search"
              value={candidateSearch}
              maxLength={128}
              onChange={(event) => {
                setCandidateSearch(event.target.value);
                setCandidateAfter(undefined);
                setChosen("");
              }}
            />
          </label>
          <ReadState
            pending={candidates.isPending}
            error={candidates.error}
            stale={candidates.isRefetchError}
          >
            <label>
              Identity
              <select
                value={chosen}
                onChange={(event) => setChosen(event.target.value)}
              >
                <option value="">Select an identity</option>
                {candidates.data?.items.map((item) => (
                  <option key={item.id} value={item.id}>
                    {item.identity.subject} · {item.identity.issuer}
                  </option>
                ))}
              </select>
            </label>
            <div className="actions">
              <button
                disabled={!candidateAfter}
                onClick={() => {
                  setCandidateAfter(undefined);
                  setChosen("");
                }}
              >
                First identities
              </button>
              <button
                disabled={!candidates.data?.nextCursor}
                onClick={() => {
                  setCandidateAfter(candidates.data?.nextCursor ?? undefined);
                  setChosen("");
                }}
              >
                More identities
              </button>
            </div>
          </ReadState>
          {selected && (
            <Command
              key={selected.id}
              title="Grant administrator access"
              permission="administrator.manage"
              method="POST"
              path="/api/v1/administrators"
              initial={{
                identity: selected.identity,
                canManageAdministrators: false,
              }}
              fields={[
                {
                  key: "canManageAdministrators",
                  label: "May manage administrators",
                  type: "checkbox",
                },
              ]}
            />
          )}
        </section>
      )}
    </>
  );
}

export function AdministratorDetail() {
  const { id } = useParams({ from: "/administrators/$id" });
  const actor = useActorKey();
  const query = useQuery({
    queryKey: [actor, "administrator", id],
    queryFn: ({ signal }) => api.administrator(id, signal),
  });
  return (
    <ReadState
      pending={query.isPending}
      error={query.error}
      stale={query.isRefetchError}
    >
      {query.data && (
        <section className="panel detail">
          <h2>{query.data.data.identity.subject}</h2>
          <dl>
            <dt>Issuer</dt>
            <dd>
              <code>{query.data.data.identity.issuer}</code>
            </dd>
            <dt>Active</dt>
            <dd>{query.data.data.active ? "Yes" : "Revoked"}</dd>
            <dt>May manage administrators</dt>
            <dd>{query.data.data.canManageAdministrators ? "Yes" : "No"}</dd>
            <dt>Updated</dt>
            <dd>{query.data.data.updatedAt}</dd>
          </dl>
          <div className="actions">
            <Command
              title="Edit administrator grant"
              permission="administrator.manage"
              method="PATCH"
              path={`/api/v1/administrators/${encodeURIComponent(id)}`}
              etag={query.data.etag}
              current={query.data.data}
              initial={{
                active: query.data.data.active,
                canManageAdministrators:
                  query.data.data.canManageAdministrators,
              }}
              fields={[
                {
                  key: "active",
                  label: "Active administrator",
                  type: "checkbox",
                },
                {
                  key: "canManageAdministrators",
                  label: "May manage administrators",
                  type: "checkbox",
                },
              ]}
              warning="Revoking denies the next browser, asset and bearer request. The last administrator who can manage administrators cannot be removed."
            />
            <Link
              to="/activity"
              search={{ targetKind: "administrator", targetId: id }}
            >
              Activity
            </Link>
          </div>
        </section>
      )}
    </ReadState>
  );
}
