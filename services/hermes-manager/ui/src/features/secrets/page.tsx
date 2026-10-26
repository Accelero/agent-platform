import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { api } from "../../api";
import { useActorKey } from "../../app/state";
import { Command, usePermitted } from "../../components/command";
import { DataTable, ReadState, usePager } from "../../components/data";

const kinds = ["env", "file", "registry"];
const writeFields = (fixedKind?: string) => [
  {
    key: "kind",
    label: "Kind",
    options: kinds,
    readOnly: !!fixedKind,
    hint: "A secret's kind is fixed once written.",
  },
  {
    key: "value",
    label: "Value",
    type: "password" as const,
    hint: "Sent over TLS to the manager, which seals it to the controller's key. It is never shown again.",
  },
];

export function SecretsPage() {
  const actor = useActorKey();
  const pager = usePager();
  const permitted = usePermitted("administer");
  const [name, setName] = useState("");
  const query = useQuery({
    queryKey: [actor, "secrets", pager.after],
    queryFn: ({ signal }) => api.secrets(pager.after, signal),
  });
  const validName = /^[a-z]([-a-z0-9]{0,61}[a-z0-9])?$/.test(name);
  return (
    <>
      <p>
        Secrets are sealed to the controller's key: the manager can store them
        but this API never returns them. Templates and assignments reference
        them by name; each new version restarts the instances that use it.
      </p>
      <ReadState
        pending={query.isPending}
        error={query.error}
        stale={query.isRefetchError}
      >
        {query.data && (
          <>
            <DataTable
              caption="Admin secrets"
              rows={query.data.items}
              empty="No admin secrets."
              columns={[
                { header: "Name", accessorKey: "name" },
                { header: "Kind", accessorKey: "kind" },
                { header: "Version", accessorKey: "version" },
                { header: "Updated", accessorKey: "updatedAt" },
                { header: "References", accessorKey: "references" },
                {
                  header: "Actions",
                  cell: ({ row }) => (
                    <div className="actions">
                      <Command
                        title={`Write new value for ${row.original.name}`}
                        permission="administer"
                        method="PUT"
                        path={`/api/v1/secrets/${encodeURIComponent(row.original.name)}`}
                        initial={{ kind: row.original.kind, value: "" }}
                        fields={writeFields(row.original.kind)}
                        sensitive
                      />
                      {row.original.references === 0 && (
                        <Command
                          title={`Delete ${row.original.name}`}
                          permission="administer"
                          method="DELETE"
                          path={`/api/v1/secrets/${encodeURIComponent(row.original.name)}`}
                          etag={`"${row.original.version}"`}
                          current={row.original}
                          bodyless
                          warning="Deletes every version. Only unreferenced secrets can be deleted."
                        />
                      )}
                    </div>
                  ),
                },
              ]}
            />
            {pager.controls(query.data.nextCursor)}
          </>
        )}
      </ReadState>
      {permitted && (
        <section className="panel detail">
          <h2>New secret</h2>
          <label>
            Name
            <input
              value={name}
              maxLength={63}
              onChange={(event) => setName(event.target.value)}
            />
          </label>
          {validName && (
            <Command
              title={`Write ${name}`}
              permission="administer"
              method="PUT"
              path={`/api/v1/secrets/${encodeURIComponent(name)}`}
              initial={{ kind: "env", value: "" }}
              fields={writeFields()}
              sensitive
            />
          )}
        </section>
      )}
    </>
  );
}
