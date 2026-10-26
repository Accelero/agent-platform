import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { api, type Schemas } from "../../api";
import { useActorKey } from "../../app/state";
import {
  DataTable,
  ListControls,
  ReadState,
  usePager,
} from "../../components/data";

function describe(source: Schemas["StorageSource"]) {
  if (source.storageClass)
    return [
      "Storage class",
      source.storageClass.name,
      "Volumes sized per assignment",
    ];
  if (source.localPath)
    return [
      "Local path",
      `${source.localPath.node}:${source.localPath.root}`,
      "Paths below the root",
    ];
  if (source.nfs)
    return [
      "NFS",
      `${source.nfs.server}:${source.nfs.root}`,
      "Paths below the export",
    ];
  return ["Unknown", "—", "—"];
}

export function StoragePage() {
  const actor = useActorKey();
  const pager = usePager();
  const sources = useQuery({
    queryKey: [actor, "storage-sources"],
    queryFn: ({ signal }) => api.storageSources(signal),
  });
  const volumes = useQuery({
    queryKey: [actor, "volumes", pager.after, pager.filters.source],
    queryFn: ({ signal }) =>
      api.volumes(pager.after, signal, pager.filters.source),
  });
  const orphans = useQuery({
    queryKey: [actor, "orphans"],
    queryFn: ({ signal }) => api.orphans(signal),
  });
  return (
    <>
      <p>
        Storage sources come from the installation's Helm values and cannot be
        changed here. Volumes from different assignments never overlap, and
        users cannot mount anything else.
      </p>
      <ReadState pending={sources.isPending} error={sources.error}>
        {sources.data && (
          <>
            <DataTable
              caption="Storage sources"
              rows={sources.data.items}
              empty="No storage sources are approved."
              columns={[
                { header: "ID", accessorKey: "id" },
                {
                  header: "Kind",
                  cell: ({ row }) => describe(row.original)[0],
                },
                {
                  header: "Where",
                  cell: ({ row }) => describe(row.original)[1],
                },
                {
                  header: "Volumes",
                  cell: ({ row }) => describe(row.original)[2],
                },
              ]}
            />
            <p>
              Data identity for shared volumes: UID{" "}
              {sources.data.dataIdentity.uid}, GID{" "}
              {sources.data.dataIdentity.gid}.
            </p>
          </>
        )}
      </ReadState>
      <ListControls
        filters={[
          {
            key: "source",
            label: "Source",
            options: sources.data?.items.map((source) => source.id) ?? [],
          },
        ]}
      />
      <ReadState
        pending={volumes.isPending}
        error={volumes.error}
        stale={volumes.isRefetchError}
      >
        {volumes.data && (
          <>
            <DataTable
              caption="Volumes"
              rows={volumes.data.items}
              empty="No volumes match."
              columns={[
                {
                  header: "Owner",
                  cell: ({ row }) => (
                    <Link
                      to="/assignments/$id"
                      params={{ id: row.original.assignmentId }}
                    >
                      {row.original.identity.subject}
                    </Link>
                  ),
                },
                { header: "Volume", accessorKey: "name" },
                { header: "Source", accessorKey: "source" },
                {
                  header: "Path or size",
                  cell: ({ row }) =>
                    row.original.path ??
                    (row.original.sizeGib
                      ? `${row.original.sizeGib} GiB`
                      : "—"),
                },
                {
                  header: "Access",
                  cell: ({ row }) =>
                    row.original.readOnly ? "read-only" : "read-write",
                },
              ]}
            />
            {pager.controls(volumes.data.nextCursor)}
          </>
        )}
      </ReadState>
      <ReadState
        pending={orphans.isPending}
        error={orphans.error}
        stale={orphans.isRefetchError}
      >
        {orphans.data && (
          <>
            <DataTable
              caption="Orphaned objects"
              rows={orphans.data.items}
              empty="No orphaned namespaces or volumes."
              columns={[
                { header: "Kind", accessorKey: "kind" },
                {
                  header: "Name",
                  cell: ({ row }) => <code>{row.original.name}</code>,
                },
                { header: "First seen", accessorKey: "firstSeenAt" },
                { header: "Last seen", accessorKey: "lastSeenAt" },
              ]}
            />
            <p>
              Namespaces and volumes with this installation's label that no
              assignment records, for example after restoring an older database.
              The manager never adopts or deletes them; remove or keep them
              yourself.{" "}
              {orphans.data.scannedAt
                ? `Last scanned ${orphans.data.scannedAt}.`
                : "The controller has not scanned yet."}
            </p>
          </>
        )}
      </ReadState>
    </>
  );
}
