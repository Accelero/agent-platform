import { TemplatePicker } from "../../components/template-picker";
import { useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { useQuery, useMutation } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { api, createIntent, type Result } from "../../api";
import { useSession, useActorKey, queryClient } from "../../app/state";
import { Command, pollInterval } from "../../components/command";
import {
  DataTable,
  ErrorNotice,
  JsonDetails,
  ReadState,
  StatusBadge,
} from "../../components/data";
import { changedPaths } from "../templates/model";
import type { components } from "../../generated/api";
type Selection = components["schemas"]["TemplateSelection"];
type Member = components["schemas"]["TemplateBatch"]["members"][number];
function SelectionField({
  value,
  onChange,
}: {
  value: string;
  onChange: (v: string) => void;
}) {
  return (
    <TemplatePicker
      value={value}
      onChange={onChange}
      label="Approved target version"
    />
  );
}
const selection = (value: string): Selection => ({
  templateId: value.split(":")[0],
  templateVersion: Number(value.split(":")[1]),
});
export function ApplyTemplate({
  assignment,
  etag,
}: {
  assignment: components["schemas"]["Assignment"];
  etag: string | null;
}) {
  const actor = useActorKey();
  const session = useSession();
  const [open, setOpen] = useState(false);
  const [target, setTarget] = useState("");
  const query = useQuery({
    queryKey: [actor, "template-preview", assignment.id, target],
    enabled: open && !!target,
    queryFn: ({ signal }) =>
      api.templatePreview(assignment.id, selection(target), signal),
  });
  if (!session.data?.permittedActions.includes("assignment.set_template"))
    return null;
  return (
    <Dialog.Root open={open} onOpenChange={setOpen}>
      <Dialog.Trigger asChild>
        <button>Apply template version</button>
      </Dialog.Trigger>
      <Dialog.Portal>
        <Dialog.Overlay className="dialog-overlay" />
        <Dialog.Content className="dialog-content">
          <Dialog.Title>Apply a pinned template version</Dialog.Title>
          <Dialog.Description>
            Review managed overrides, storage, resources and Recreate restart
            effects. Ownership, instance identity, retained data and credentials
            stay with this exact subject.
          </Dialog.Description>
          <SelectionField value={target} onChange={setTarget} />
          {target && (
            <ReadState
              pending={query.isPending}
              error={query.error}
              stale={query.isRefetchError}
            >
              {query.data && (
                <>
                  <JsonDetails
                    title="Current configuration"
                    value={query.data.currentDefinition}
                  />
                  <JsonDetails
                    title="Proposed configuration"
                    value={query.data.proposedDefinition}
                  />
                  <JsonDetails
                    title="Changed managed-setting paths"
                    value={changedPaths(
                      query.data.currentDefinition?.adminConfig,
                      query.data.proposedDefinition.adminConfig,
                    )}
                  />
                  <p>
                    {query.data.restart
                      ? "Applying drains active execution and restarts this Hermes instance using Recreate."
                      : "The assignment stays disabled. Its selected version is updated for future reconciliation."}
                  </p>
                  {query.data.eligible &&
                  query.data.expectedVersion === assignment.version ? (
                    <Command
                      key={target}
                      title="Confirm template application"
                      permission="assignment.set_template"
                      method="POST"
                      path={`/api/v1/assignments/${assignment.id}/template`}
                      initial={
                        selection(target) as unknown as Record<string, unknown>
                      }
                      etag={etag}
                      current={query.data}
                      warning="Apply only to this exact assignment. Storage-class changes and shrinking require a separate migration and cannot dispatch."
                    />
                  ) : (
                    <p role="alert">
                      Application blocked:{" "}
                      {query.data.blockedReasons.join(", ") ||
                        "The assignment changed. Refresh and review it again."}
                    </p>
                  )}
                </>
              )}
            </ReadState>
          )}
          <Dialog.Close asChild>
            <button>Close template review</button>
          </Dialog.Close>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
export function BulkApply({ members }: { members: Member[] }) {
  const actor = useActorKey();
  const session = useSession();
  const [open, setOpen] = useState(false);
  const [target, setTarget] = useState("");
  const [intent] = useState(() => createIntent());
  const [previewIntent] = useState(() => createIntent());
  const [started, setStarted] = useState(0);
  const input = { selection: selection(target), members };
  const preview = useMutation({
    mutationFn: () =>
      previewIntent(
        "POST",
        "/api/v1/template-batches/preview",
        input,
        session.data!.csrfToken!,
      ) as Promise<Result<"previewTemplateBatch">>,
  });
  const apply = useMutation({
    mutationFn: () =>
      intent(
        "POST",
        "/api/v1/template-batches",
        input,
        session.data!.csrfToken!,
      ) as Promise<Result<"applyTemplateBatch">>,
    onSuccess: () => {
      setStarted(Date.now());
      void queryClient.invalidateQueries();
    },
  });
  const result = useQuery({
    queryKey: [actor, "template-batch", apply.data?.id],
    enabled: open && !!apply.data?.id,
    queryFn: ({ signal }) => api.templateBatch(apply.data!.id, signal),
    refetchOnWindowFocus: true,
    refetchInterval: (query) =>
      pollInterval(
        started,
        Date.now(),
        query.state.dataUpdateCount,
        query.state.data?.state,
      ),
  });
  if (!session.data?.permittedActions.includes("assignment.set_template"))
    return null;
  return (
    <Dialog.Root
      open={open}
      onOpenChange={(value) => {
        setOpen(value);
        if (!value) {
          preview.reset();
          apply.reset();
          setTarget("");
        }
      }}
    >
      <Dialog.Trigger asChild>
        <button disabled={!members.length}>
          Apply to {members.length} selected assignments
        </button>
      </Dialog.Trigger>
      <Dialog.Portal>
        <Dialog.Overlay className="dialog-overlay" />
        <Dialog.Content className="dialog-content">
          <Dialog.Title>Review explicit bulk selection</Dialog.Title>
          <Dialog.Description>
            Only the selected assignment IDs below are included. Maximum 50.
            Each command retains its own precondition, result and idempotent
            retry. Recreate drains and restarts eligible enabled instances.
          </Dialog.Description>
          <JsonDetails
            title="Exact selected assignments and versions"
            value={members}
          />
          {!apply.data && (
            <SelectionField
              value={target}
              onChange={(value) => {
                setTarget(value);
                preview.reset();
              }}
            />
          )}
          {!apply.data && (
            <>
              <button
                disabled={!target || preview.isPending || members.length > 50}
                onClick={() => preview.mutate()}
              >
                Preview selected assignments
              </button>
              {preview.error && <ErrorNotice error={preview.error} />}
              {preview.data && (
                <>
                  <JsonDetails
                    title="Per-assignment eligibility and configuration"
                    value={preview.data.items}
                  />
                  <button
                    disabled={
                      apply.isPending ||
                      preview.data.items.some((item) => !item.eligible)
                    }
                    onClick={() => apply.mutate()}
                  >
                    Apply reviewed selection
                  </button>
                </>
              )}
            </>
          )}
          {apply.error && (
            <>
              <ErrorNotice error={apply.error} />
              <button onClick={() => apply.mutate()}>
                Retry identical batch
              </button>
            </>
          )}
          {apply.data && (
            <>
              <p role="status">
                Batch accepted. Per-user outcomes below describe command
                dispatch; instance rollout may still be pending.
              </p>
              <Link to="/template-batches/$id" params={{ id: apply.data.id }}>
                Open durable batch results
              </Link>
              <BatchRows value={result.data} />
              {result.data && (
                <ResumeBatch
                  value={result.data}
                  onSuccess={() => {
                    setStarted(Date.now());
                    void result.refetch();
                  }}
                />
              )}
            </>
          )}
          <Dialog.Close asChild>
            <button>Close bulk review</button>
          </Dialog.Close>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
export function BatchRows({ value }: { value?: Result<"getTemplateBatch"> }) {
  return value ? (
    <DataTable
      caption="Per-user dispatch outcomes"
      rows={value.items}
      empty="No batch members."
      columns={[
        {
          header: "Assignment",
          cell: ({ row }) => (
            <Link
              to="/assignments/$id"
              params={{ id: row.original.assignmentId }}
            >
              {row.original.assignmentId}
            </Link>
          ),
        },
        {
          header: "Dispatch state",
          cell: ({ row }) => <StatusBadge value={row.original.state} />,
        },
        { header: "Result", accessorKey: "errorCode" },
        {
          header: "Rollout operation",
          cell: ({ row }) =>
            row.original.operationId ? (
              <Link
                to="/operations/$id"
                params={{ id: row.original.operationId }}
              >
                Inspect rollout
              </Link>
            ) : (
              "No operation accepted"
            ),
        },
      ]}
    />
  ) : null;
}

export function ResumeBatch({
  value,
  onSuccess,
}: {
  value: Result<"getTemplateBatch">;
  onSuccess: () => void;
}) {
  const session = useSession();
  const [intent] = useState(() => createIntent());
  const resume = useMutation({
    mutationFn: () =>
      intent(
        "POST",
        `/api/v1/template-batches/${value.id}/resume`,
        {},
        session.data!.csrfToken!,
      ),
    onSuccess,
  });
  if (
    value.state !== "Pending" ||
    !session.data?.permittedActions.includes("assignment.set_template") ||
    value.actor.issuer !== session.data.actor.issuer ||
    value.actor.subject !== session.data.actor.subject
  )
    return null;
  return (
    <>
      <button disabled={resume.isPending} onClick={() => resume.mutate()}>
        Resume pending dispatch
      </button>
      {resume.error && <ErrorNotice error={resume.error} />}
    </>
  );
}
