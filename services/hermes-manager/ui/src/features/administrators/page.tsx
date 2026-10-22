import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Link, useParams } from "@tanstack/react-router";
import { api } from "../../api";
import { useSession, useActorKey } from "../../app/state";
import { Command } from "../../components/command";
import {
  DataTable,
  ListControls,
  ReadState,
  JsonDetails,
  usePager,
} from "../../components/data";
export function AdministratorsPage() {
  const actor = useActorKey();
  const session = useSession();
  const pager = usePager();
  const [identity, setIdentity] = useState("");
  const [candidateSearch, setCandidateSearch] = useState("");
  const [candidateAfter, setCandidateAfter] = useState<string>();
  const query = useQuery({
    queryKey: [actor, "administrators", pager.after, pager.filters],
    queryFn: ({ signal }) =>
      api.administrators(pager.after, signal, pager.filters),
  });
  const candidates = useQuery({
    queryKey: [
      actor,
      "administrator-candidates",
      candidateSearch,
      candidateAfter,
    ],
    enabled: session.data?.permittedActions.includes("administrator.manage"),
    queryFn: ({ signal }) =>
      api.administratorCandidates(signal, candidateAfter, candidateSearch),
  });
  const selected = candidates.data?.items.find(
    (item) => JSON.stringify(item) === identity,
  );
  return (
    <>
      <p>
        Keycloak authenticates exact identities. Active PostgreSQL grants are
        the sole source of administrator permission; token roles do not grant
        access. Revocation denies new requests immediately, including assets and
        bearer tokens. The browser clears its cached state when its next session
        check runs, within 60 seconds.
      </p>
      <ListControls kind="administrators" />
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
                      params={{ id: row.original.id }}
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
                { header: "Authority", accessorKey: "source" },
              ]}
            />
            {pager.controls(query.data.nextCursor)}
          </>
        )}
      </ReadState>
      {session.data?.permittedActions.includes("administrator.manage") && (
        <section className="panel detail">
          <h2>Add an administrator</h2>
          <p>
            Select an exact identity verified through a signed manager-audience
            login. Users can sign in once to establish this evidence without
            gaining administrator access.
          </p>
          <label>
            Search verified identities
            <input
              type="search"
              value={candidateSearch}
              maxLength={128}
              onChange={(event) => {
                setCandidateSearch(event.target.value);
                setCandidateAfter(undefined);
                setIdentity("");
              }}
            />
          </label>
          <ReadState
            pending={candidates.isPending}
            error={candidates.error}
            stale={candidates.isRefetchError}
          >
            <label>
              Verified identity
              <select
                value={identity}
                onChange={(event) => setIdentity(event.target.value)}
              >
                <option value="">Select an identity</option>
                {candidates.data?.items.map((item) => (
                  <option
                    key={JSON.stringify(item)}
                    value={JSON.stringify(item)}
                  >
                    {item.subject} · {item.issuer}
                  </option>
                ))}
              </select>
            </label>
            <div className="actions">
              <button
                disabled={!candidateAfter}
                onClick={() => {
                  setCandidateAfter(undefined);
                  setIdentity("");
                }}
              >
                First identities
              </button>
              <button
                disabled={!candidates.data?.nextCursor}
                onClick={() => {
                  setCandidateAfter(candidates.data?.nextCursor ?? undefined);
                  setIdentity("");
                }}
              >
                More identities
              </button>
            </div>
          </ReadState>
          {selected && !candidates.isError && (
            <Command
              key={identity}
              title="Grant administrator access"
              permission="administrator.manage"
              method="POST"
              path="/api/v1/administrators"
              initial={{
                identity: selected,
                active: true,
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
  const { id } = useParams({ strict: false });
  const actor = useActorKey();
  const query = useQuery({
    queryKey: [actor, "administrator", id],
    queryFn: ({ signal }) => api.administrator(id!, signal),
  });
  return (
    <ReadState
      pending={query.isPending}
      error={query.error}
      stale={query.isRefetchError}
    >
      {query.data && (
        <>
          <JsonDetails title="Administrator grant" value={query.data.data} />
          <Command
            title="Edit administrator grant"
            permission="administrator.manage"
            method="PATCH"
            path={`/api/v1/administrators/${id}`}
            etag={query.data.etag}
            current={query.data.data}
            initial={{
              identity: query.data.data.identity,
              active: query.data.data.active,
              canManageAdministrators: query.data.data.canManageAdministrators,
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
            warning="Revoking immediately denies browser, asset and bearer API access. The last administrator with recovery-management permission cannot be removed."
          />
        </>
      )}
    </ReadState>
  );
}
