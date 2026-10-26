import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Link, useParams } from "@tanstack/react-router";
import { api, versionLabel, type Instance } from "../../api";
import { useActorKey } from "../../app/state";
import { Command } from "../../components/command";
import {
  CopyId,
  DataTable,
  JsonDetails,
  ListControls,
  ReadState,
  StatusBadge,
  usePager,
} from "../../components/data";
import { ChangeAssignment } from "./editor";
import { Rollout } from "./rollout";

export function AssignmentsPage() {
  const actor = useActorKey();
  const pager = usePager();
  const [selected, setSelected] = useState<Map<string, number>>(new Map());
  const query = useQuery({
    queryKey: [actor, "assignments", pager.after, pager.filters],
    queryFn: ({ signal }) =>
      api.assignments(pager.after, signal, pager.filters),
  });
  const toggle = (id: string, version: number) =>
    setSelected((current) => {
      const next = new Map(current);
      if (next.has(id)) next.delete(id);
      else if (next.size < 50) next.set(id, version);
      return next;
    });
  return (
    <>
      <p>
        An assignment gives one exact identity its own Hermes instance. Create
        one from <Link to="/users">Users</Link>.
      </p>
      <ListControls
        search="Search assignments"
        filters={[
          {
            key: "access",
            label: "Access",
            options: ["enabled", "suspended", "revoked"],
          },
          { key: "templateId", label: "Template ID", maxLength: 63 },
        ]}
      />
      <ReadState
        pending={query.isPending}
        error={query.error}
        stale={query.isRefetchError}
      >
        {query.data && (
          <>
            <DataTable
              caption="Assignments"
              rows={query.data.items}
              empty="No assignments match."
              columns={[
                {
                  header: "Select",
                  cell: ({ row }) => (
                    <input
                      type="checkbox"
                      aria-label={`Select ${row.original.identity.subject}`}
                      checked={selected.has(row.original.id)}
                      onChange={() =>
                        toggle(row.original.id, row.original.version)
                      }
                    />
                  ),
                },
                {
                  header: "Identity",
                  cell: ({ row }) => (
                    <Link
                      to="/assignments/$id"
                      params={{ id: row.original.id }}
                    >
                      {row.original.identity.subject}
                      <small>{row.original.identity.issuer}</small>
                    </Link>
                  ),
                },
                {
                  header: "Access",
                  cell: ({ row }) => (
                    <StatusBadge value={row.original.access} />
                  ),
                },
                {
                  header: "Template",
                  cell: ({ row }) => versionLabel(row.original.template),
                },
                {
                  header: "Instance",
                  cell: ({ row }) => <StatusBadge value={row.original.phase} />,
                },
                { header: "Version", accessorKey: "version" },
              ]}
            />
            {pager.controls(query.data.nextCursor)}
          </>
        )}
      </ReadState>
      <Rollout selected={selected} onDone={() => setSelected(new Map())} />
    </>
  );
}

export function InstanceStatus({ instance }: { instance: Instance }) {
  return (
    <section className="panel detail" aria-label="Instance">
      <h2>Instance</h2>
      <dl>
        <dt>Phase</dt>
        <dd>
          <StatusBadge value={instance.phase} />
          {instance.blockedReason && <code>{instance.blockedReason}</code>}
        </dd>
        <dt>Namespace</dt>
        <dd>
          <code>{instance.namespace}</code>
        </dd>
        <dt>Desired / observed generation</dt>
        <dd>
          {instance.desiredGeneration} / {instance.observedGeneration}
          {instance.observedGeneration < instance.desiredGeneration &&
            " — the controller has not applied the latest change yet"}
        </dd>
        <dt>Execution generation</dt>
        <dd>{instance.executionGeneration}</dd>
        <dt>Observed</dt>
        <dd>{instance.observedAt ?? "Not yet observed"}</dd>
      </dl>
      {instance.conditions.length > 0 && (
        <table>
          <caption>Conditions</caption>
          <thead>
            <tr>
              <th scope="col">Condition</th>
              <th scope="col">Status</th>
              <th scope="col">Reason</th>
            </tr>
          </thead>
          <tbody>
            {instance.conditions.map((condition) => (
              <tr key={condition.type}>
                <td>{condition.type}</td>
                <td>{condition.status}</td>
                <td>
                  <code>{condition.reason}</code>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </section>
  );
}

const failures: Record<string, string> = {
  image_unavailable: "Image cannot be pulled",
  start_failed: "Container failed to start",
  contract_failed: "Worker broke the SSH contract",
};

/** Terminal environments of one instance; a failed worker shows its reason. */
export function Environments({ instanceId }: { instanceId: string }) {
  const actor = useActorKey();
  const query = useQuery({
    queryKey: [actor, "environments", instanceId],
    queryFn: ({ signal }) => api.environments(instanceId, signal),
    refetchInterval: 10_000,
  });
  return (
    <ReadState
      pending={query.isPending}
      error={query.error}
      stale={query.isRefetchError}
    >
      {query.data && (
        <DataTable
          caption="Terminal environments"
          rows={query.data.items}
          empty="No terminal environments."
          columns={[
            {
              header: "Backend",
              cell: ({ row }) => versionLabel(row.original.backend),
            },
            {
              header: "State",
              cell: ({ row }) => <StatusBadge value={row.original.state} />,
            },
            {
              header: "Failure",
              cell: ({ row }) =>
                row.original.failure ? (
                  <span title={row.original.failure}>
                    {failures[row.original.failure] ?? row.original.failure}
                  </span>
                ) : (
                  "—"
                ),
            },
            {
              header: "Session",
              cell: ({ row }) => row.original.session ?? row.original.mode,
            },
            { header: "Generation", accessorKey: "generation" },
            { header: "Leases", accessorKey: "leases" },
            { header: "Created", accessorKey: "createdAt" },
          ]}
        />
      )}
    </ReadState>
  );
}

export function AssignmentDetail() {
  const { id } = useParams({ from: "/assignments/$id" });
  const actor = useActorKey();
  const query = useQuery({
    queryKey: [actor, "assignment", id],
    queryFn: ({ signal }) => api.assignment(id, signal),
    refetchInterval: (state) =>
      state.state.data &&
      state.state.data.data.instance.observedGeneration <
        state.state.data.data.instance.desiredGeneration
        ? 5_000
        : false,
  });
  return (
    <ReadState
      pending={query.isPending}
      error={query.error}
      stale={query.isRefetchError}
    >
      {query.data &&
        (() => {
          const { data: assignment, etag } = query.data;
          const spec = assignment.spec;
          return (
            <>
              <section className="panel detail">
                <h2>{assignment.identity.subject}</h2>
                <dl>
                  <dt>Issuer</dt>
                  <dd>
                    <code>{assignment.identity.issuer}</code>
                  </dd>
                  <dt>Assignment</dt>
                  <dd>
                    <CopyId value={assignment.id} />
                  </dd>
                  <dt>Access</dt>
                  <dd>
                    <StatusBadge value={spec.access} />
                  </dd>
                  <dt>Template</dt>
                  <dd>
                    <Link
                      to="/templates/$id"
                      params={{ id: spec.template.id }}
                      search={{ version: String(spec.template.version) }}
                    >
                      {versionLabel(spec.template)}
                    </Link>
                  </dd>
                  <dt>Terminal backends</dt>
                  <dd>
                    {Object.entries(spec.terminal?.allowed ?? {}).length
                      ? Object.entries(spec.terminal?.allowed ?? {}).map(
                          ([backend, version]) => (
                            <Link
                              key={backend}
                              to="/terminal-backends/$id"
                              params={{ id: backend }}
                              search={{ version: String(version) }}
                            >
                              {versionLabel({ id: backend, version })}
                              {spec.terminal?.default === backend &&
                                " (default)"}
                            </Link>
                          ),
                        )
                      : "None allowed"}
                  </dd>
                  <dt>Environment lifetime</dt>
                  <dd>{spec.terminal?.mode ?? "per_session"}</dd>
                  <dt>Version</dt>
                  <dd>{assignment.version}</dd>
                </dl>
                <div className="actions">
                  <ChangeAssignment
                    id={assignment.id}
                    spec={spec}
                    etag={etag}
                  />
                  <Command
                    title="Retry instance"
                    permission="administer"
                    method="POST"
                    path={`/api/v1/assignments/${encodeURIComponent(assignment.id)}/retry`}
                    etag={etag}
                    current={assignment}
                    bodyless
                    warning="Clears a blocked instance's retry backoff so the controller tries again now."
                  />
                  <Link
                    to="/activity"
                    search={{
                      targetKind: "assignment",
                      targetId: assignment.id,
                    }}
                  >
                    Activity
                  </Link>
                </div>
              </section>
              <InstanceStatus instance={assignment.instance} />
              <Environments instanceId={assignment.instance.id} />
              <DataTable
                caption="Volumes"
                rows={Object.entries(spec.volumes ?? {})}
                empty="No volumes besides the private Hermes home."
                columns={[
                  { header: "Name", cell: ({ row }) => row.original[0] },
                  {
                    header: "Source",
                    cell: ({ row }) => row.original[1].source,
                  },
                  {
                    header: "Path or size",
                    cell: ({ row }) =>
                      row.original[1].path ??
                      (row.original[1].sizeGib
                        ? `${row.original[1].sizeGib} GiB`
                        : "—"),
                  },
                  {
                    header: "Access",
                    cell: ({ row }) =>
                      row.original[1].readOnly ? "read-only" : "read-write",
                  },
                ]}
              />
              {spec.adminConfig && (
                <JsonDetails
                  title="Admin settings overrides"
                  value={spec.adminConfig}
                />
              )}
              {spec.secrets && spec.secrets.length > 0 && (
                <DataTable
                  caption="Secret references"
                  rows={spec.secrets}
                  empty=""
                  columns={[
                    { header: "Secret", accessorKey: "name" },
                    {
                      header: "Delivered as",
                      cell: ({ row }) =>
                        row.original.env
                          ? `env ${row.original.env}`
                          : `file ${row.original.file}`,
                    },
                  ]}
                />
              )}
            </>
          );
        })()}
    </ReadState>
  );
}
