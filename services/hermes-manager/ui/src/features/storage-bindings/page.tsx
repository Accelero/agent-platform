import { StorageCommand } from "./commands";
import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Link, useNavigate, useParams } from "@tanstack/react-router";
import { api } from "../../api";
import { useActorKey } from "../../app/state";
import {
  DataTable,
  JsonDetails,
  ReadState,
  usePager,
  ListControls,
} from "../../components/data";
export function StoragePage() {
  const actor = useActorKey();
  const pager = usePager();
  const navigate = useNavigate();
  const [instanceId, setInstanceId] = useState(pager.instanceId ?? "");
  const query = useQuery({
    queryKey: [actor, "storage", pager.instanceId, pager.after, pager.filters],
    enabled: !!pager.instanceId,
    queryFn: ({ signal }) =>
      api.bindingReferences(
        pager.instanceId!,
        pager.filters.q ?? "",
        signal,
        pager.after,
        pager.filters,
      ),
  });
  return (
    <>
      <StorageCommand instanceId={pager.instanceId} />
      <form
        onSubmit={(event) => {
          event.preventDefault();
          void navigate({
            to: "/storage-bindings",
            search: { instanceId, cursors: [] },
          });
        }}
      >
        <label>
          Instance ID
          <input
            required
            pattern="[0-9a-fA-F-]{36}"
            value={instanceId}
            onChange={(event) => setInstanceId(event.target.value)}
          />
        </label>
        <button>Inspect storage</button>
      </form>
      {pager.instanceId && <ListControls kind="bindings" />}
      {pager.instanceId && (
        <ReadState
          pending={query.isPending}
          error={query.error}
          stale={query.isRefetchError}
        >
          {query.data && (
            <>
              <DataTable
                caption="Instance storage bindings"
                rows={query.data.items}
                empty="No storage bindings are registered for this instance."
                columns={[
                  {
                    header: "Binding",
                    cell: ({ row }) => (
                      <Link
                        to="/storage-bindings/$id"
                        params={{ id: row.original.id }}
                      >
                        {row.original.id}
                      </Link>
                    ),
                  },
                  { header: "PVC", accessorKey: "pvcName" },
                  { header: "Namespace", accessorKey: "namespace" },
                  { header: "Version", accessorKey: "version" },
                ]}
              />
              {pager.controls(query.data.nextCursor)}
            </>
          )}
        </ReadState>
      )}
    </>
  );
}
export function StorageDetail() {
  const { id } = useParams({ strict: false });
  const actor = useActorKey();
  const query = useQuery({
    queryKey: [actor, "storage-binding", id],
    queryFn: ({ signal }) => api.storageBinding(id!, signal),
  });
  return (
    <ReadState
      pending={query.isPending}
      error={query.error}
      stale={query.isRefetchError}
    >
      {query.data && (
        <>
          <StorageCommand binding={query.data.data} etag={query.data.etag} />
          <JsonDetails title="Storage binding" value={query.data.data} />
        </>
      )}
    </ReadState>
  );
}
