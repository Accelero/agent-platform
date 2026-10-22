import { ListControls } from "../../components/data";
import { RegisterRuntime } from "./commands";
import { useQuery } from "@tanstack/react-query";
import { Link, useParams } from "@tanstack/react-router";
import { api } from "../../api";
import { useActorKey } from "../../app/state";
import {
  DataTable,
  ReadState,
  usePager,
  JsonDetails,
} from "../../components/data";
export function RuntimesPage() {
  const pager = usePager();
  const actor = useActorKey();
  const query = useQuery({
    queryKey: [actor, "runtimes", pager.after, pager.filters],
    queryFn: ({ signal }) => api.runtimes(pager.after, signal, pager.filters),
  });
  return (
    <>
      <RegisterRuntime />
      <>
        <ListControls kind="runtimes" />
        <ReadState
          pending={query.isPending}
          error={query.error}
          stale={query.isRefetchError}
        >
          {query.data && (
            <>
              <DataTable
                caption="Approved terminal types"
                rows={query.data.items}
                empty="No terminal backends are registered yet."
                columns={[
                  {
                    header: "Runtime ID",
                    cell: ({ row }) => (
                      <Link
                        to="/terminal-runtimes/$id"
                        params={{ id: row.original.runtime.id }}
                      >
                        {row.original.runtime.id}
                      </Link>
                    ),
                  },
                  {
                    header: "Version",
                    accessorFn: (row) => row.runtime.version,
                  },
                  { header: "Image", accessorKey: "image" },
                  { header: "SSH contract", accessorKey: "sshContract" },
                  {
                    header: "CPU / memory",
                    cell: ({ row }) =>
                      `${row.original.resources.cpuMillis} mCPU / ${row.original.resources.memoryMib} MiB`,
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
export function RuntimeDetail() {
  const { id } = useParams({ strict: false });
  const actor = useActorKey();
  const pager = usePager();
  const latest = useQuery({
    queryKey: [actor, "runtime-latest", id],
    queryFn: ({ signal }) => api.runtime(id!, signal),
  });
  const query = useQuery({
    queryKey: [actor, "runtime-versions", id, pager.after, pager.filters],
    queryFn: ({ signal }) =>
      api.runtimeHistory(id!, pager.after, signal, pager.filters),
  });
  return (
    <>
      <RegisterRuntime
        id={id}
        nextVersion={
          latest.data?.runtime?.version
            ? latest.data.runtime.version + 1
            : undefined
        }
      />
      <Link to="/terminal-runtimes">← Terminal backends</Link>
      <ListControls kind="versions" />
      <ReadState
        pending={query.isPending}
        error={query.error}
        stale={query.isRefetchError}
      >
        {query.data && (
          <>
            <DataTable
              caption={`Versions of ${id}`}
              rows={query.data.items}
              empty="No runtime versions are registered yet."
              columns={[
                { header: "Version", accessorFn: (row) => row.runtime.version },
                { header: "Image", accessorKey: "image" },
                { header: "Contract", accessorKey: "sshContract" },
                {
                  header: "User / group",
                  cell: ({ row }) =>
                    `${row.original.uid} / ${row.original.gid}`,
                },
              ]}
            />
            {pager.controls(query.data.nextCursor)}
            {query.data.items.map((definition) => (
              <section
                className="panel detail"
                key={definition.runtime.version}
              >
                <h2>
                  Runtime {definition.runtime.id} · v
                  {definition.runtime.version}
                </h2>
                <JsonDetails
                  title="Complete immutable runtime definition"
                  value={definition}
                />
                <Link
                  to="/assignments"
                  search={{
                    runtimeId: definition.runtime.id,
                    runtimeVersion: String(definition.runtime.version),
                    cursors: [],
                  }}
                >
                  Inspect assignments allowed to request this version →
                </Link>
                <p>
                  Owner-specific storage and pull-secret references are
                  inspected through each assignment; runtime registration does
                  not grant access.
                </p>
                <RegisterRuntime
                  id={id}
                  definition={definition}
                  nextVersion={
                    latest.data?.runtime?.version
                      ? latest.data.runtime.version + 1
                      : undefined
                  }
                />
              </section>
            ))}
          </>
        )}
      </ReadState>
    </>
  );
}
