import { useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { useQuery } from "@tanstack/react-query";
import { api } from "../../api";
import { useActorKey, useSession } from "../../app/state";
import { Command, type Field } from "../../components/command";
import type { components } from "../../generated/api";
import { ReadState, JsonDetails } from "../../components/data";
import {
  resourcesFields,
  resourcesInitial,
  mountFields,
  limitsFields,
  limitsInitial,
} from "../terminal-runtimes/commands";
export function CreateAssignment({
  initialIdentity,
  disabled = false,
}: {
  initialIdentity?: { issuer: string; subject: string };
  disabled?: boolean;
} = {}) {
  const session = useSession();
  const actor = useActorKey();
  const [open, setOpen] = useState(false);
  const [identity, setIdentity] = useState(
    initialIdentity ?? { issuer: "", subject: "" },
  );
  const [after, setAfter] = useState<string>();
  const identities = useQuery({
    queryKey: [actor, "identity-picker", after],
    enabled: open,
    queryFn: ({ signal }) => api.identities(after, signal),
  });
  const templates = useQuery({
    queryKey: [actor, "templates"],
    enabled: open,
    queryFn: ({ signal }) => api.templates(signal),
  });
  if (!session.data?.permittedActions.includes("assignment.create"))
    return null;
  const fields: Field[] = [
    {
      key: "identity",
      label: "Exact identity",
      fields: [
        { key: "issuer", label: "Issuer" },
        { key: "subject", label: "Subject" },
      ],
    },
    {
      key: "template",
      template: true,
      label: "Approved template version",
      options:
        templates.data?.items.map((item) => `${item.id} · ${item.version}`) ??
        [],
    },
    {
      key: "access",
      label: "Initial access",
      options: ["enabled", "suspended", "revoked"],
    },
  ];
  return (
    <Dialog.Root open={open} onOpenChange={setOpen}>
      <Dialog.Trigger asChild>
        <button disabled={disabled}>Create assignment</button>
      </Dialog.Trigger>
      <Dialog.Portal>
        <Dialog.Overlay className="dialog-overlay" />
        <Dialog.Content className="dialog-content">
          <Dialog.Title>Select an identity</Dialog.Title>
          <Dialog.Description>
            Select a recorded identity or enter an exact issuer and subject in
            the assignment form.
          </Dialog.Description>
          <ReadState
            pending={identities.isPending || templates.isPending}
            error={identities.error ?? templates.error}
          >
            {identities.data && (
              <>
                <label>
                  Recorded identity
                  <select
                    value=""
                    onChange={(event) => {
                      const item = identities.data!.items.find(
                        (item) => item.id === event.target.value,
                      );
                      if (item) setIdentity(item.identity);
                    }}
                  >
                    <option value="">Select or enter manually</option>
                    {identities.data.items.map((item) => (
                      <option value={item.id} key={item.id}>
                        {item.identity.subject} — {item.identity.issuer}
                      </option>
                    ))}
                  </select>
                </label>
                <div className="pager">
                  <button disabled={!after} onClick={() => setAfter(undefined)}>
                    First identities
                  </button>
                  <button
                    disabled={!identities.data.nextCursor}
                    onClick={() =>
                      setAfter(identities.data!.nextCursor ?? undefined)
                    }
                  >
                    More identities
                  </button>
                </div>
                <Command
                  key={JSON.stringify(identity)}
                  title="Create personal instance"
                  permission="assignment.create"
                  method="POST"
                  path="/api/v1/assignments"
                  initial={{
                    identity,
                    template: templates.data?.items[0]
                      ? `${templates.data.items[0].id}:${templates.data.items[0].version}`
                      : "",
                    access: "enabled",
                  }}
                  fields={fields}
                  renderReview={(body) => (
                    <CreationTemplateReview selected={String(body.template)} />
                  )}
                  preflight={async (body) => {
                    const [id, version] = String(body.template).split(":");
                    await api.templateVersion(id, Number(version));
                  }}
                  transform={(body) => {
                    const [templateId, version] = String(body.template).split(
                      ":",
                    );
                    return {
                      identity: body.identity,
                      access: body.access,
                      templateId,
                      templateVersion: Number(version),
                    };
                  }}
                />
              </>
            )}
          </ReadState>
          <Dialog.Close asChild>
            <button>Close identity picker</button>
          </Dialog.Close>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
export function AssignmentCommands({
  assignment,
  etag,
  execution,
  instanceEtag,
}: {
  assignment: components["schemas"]["Assignment"];
  etag: string | null;
  execution?: components["schemas"]["ExecutionPolicyView"];
  instanceEtag?: string | null;
}) {
  if (assignment.source === "infrastructure")
    return (
      <p>
        Infrastructure-owned assignment. Change its desired state through the
        infrastructure source.
      </p>
    );
  const id = assignment.id;
  const desired = execution?.desired;
  const policy = {
    defaultRuntimeId: desired?.defaultRuntimeId ?? "",
    allowed:
      desired?.runtimes.map((item) => ({
        runtime: item.definition.runtime,
        resources: item.resources,
        mounts: item.mounts.map((mount) => ({
          bindingId: mount.binding.id,
          bindingVersion: mount.binding.version,
          mount: mount.mount,
        })),
        limits: item.limits ?? limitsInitial,
      })) ?? [],
  };
  return (
    <div className="actions">
      <Command
        title="Change access"
        permission="assignment.set_access"
        method="PATCH"
        path={`/api/v1/assignments/${id}`}
        initial={{ access: assignment.access }}
        fields={[
          {
            key: "access",
            label: "Access",
            options: ["enabled", "suspended", "revoked"],
          },
        ]}
        etag={etag}
        current={assignment}
        warning="Suspending or revoking denies routing immediately and retains instance data. Enabling may commission or restart the instance. Review the selected access before confirming."
      />
      <Command
        title="Retry reconciliation"
        permission="instance.retry"
        method="POST"
        path={`/api/v1/instances/${assignment.instanceId}/reconcile`}
        initial={{}}
        etag={instanceEtag ?? null}
        current={assignment}
        warning="Retry the current desired state. This does not enable suspended or revoked access."
      />
      {execution && (
        <Command
          title="Edit execution policy"
          permission="execution.policy.set"
          method="PATCH"
          path={`/api/v1/assignments/${id}/execution`}
          etag={etag}
          current={execution}
          initial={policy}
          fields={[
            {
              key: "defaultRuntimeId",
              label: "Default runtime ID",
              reference: "runtime",
            },
            {
              key: "allowed",
              label: "Allowed runtime",
              max: 16,
              item: {
                key: "",
                label: "",
                fields: [
                  {
                    key: "runtime",
                    label: "Runtime",
                    fields: [
                      {
                        key: "id",
                        label: "Runtime ID",
                        reference: "runtime",
                        versionKey: "version",
                      },
                      {
                        key: "version",
                        label: "Version",
                        type: "number",
                        min: 1,
                      },
                    ],
                  },
                  {
                    key: "resources",
                    label: "Resources",
                    fields: resourcesFields,
                  },
                  { key: "limits", label: "Limits", fields: limitsFields },
                  {
                    key: "mounts",
                    label: "Mount selection",
                    item: {
                      key: "",
                      label: "",
                      fields: [
                        {
                          key: "bindingId",
                          label: "Binding ID",
                          reference: "binding",
                          referenceInstanceId: assignment.instanceId,
                          versionKey: "bindingVersion",
                        },
                        {
                          key: "bindingVersion",
                          label: "Binding version",
                          type: "number",
                          min: 1,
                        },
                        { key: "mount", label: "Mount", fields: mountFields },
                      ],
                    },
                    initial: {
                      bindingId: "",
                      bindingVersion: 1,
                      mount: { path: "", readOnly: true },
                    },
                  },
                ],
              },
              initial: {
                runtime: { id: "", version: 1 },
                resources: resourcesInitial,
                limits: limitsInitial,
                mounts: [],
              },
            },
          ]}
        />
      )}
      <Command
        title="Bind pull secret"
        permission="execution.pull-secret.register"
        method="POST"
        path={`/api/v1/instances/${assignment.instanceId}/pull-secret-bindings`}
        initial={{
          instanceId: assignment.instanceId,
          namespace: assignment.namespace,
          namespaceUid: "",
          name: "",
          uid: "",
        }}
        fields={[
          { key: "instanceId", label: "Instance ID", readOnly: true },
          { key: "namespace", label: "Namespace", readOnly: true },
          { key: "namespaceUid", label: "Namespace UID" },
          { key: "name", label: "Existing secret name" },
          { key: "uid", label: "Existing secret UID" },
        ]}
        warning="Register an existing approved secret reference. Supply names and UIDs only; credential values stay outside the dashboard."
      />
    </div>
  );
}

function CreationTemplateReview({ selected }: { selected: string }) {
  const actor = useActorKey();
  const [id, version] = selected.split(":");
  const query = useQuery({
    queryKey: [actor, "template-version", id, Number(version)],
    enabled: !!id && Number(version) > 0,
    queryFn: ({ signal }) => api.templateVersion(id, Number(version), signal),
  });
  return (
    <ReadState
      pending={query.isPending && !!selected}
      error={query.error}
      stale={query.isRefetchError}
    >
      {query.data && (
        <>
          <JsonDetails
            title="Selected template deployment and managed configuration"
            value={query.data.definition}
          />
          <p>
            Creating this assignment commissions a new instance for the exact
            issuer and subject above. Managed execution and the required plugin
            remain enforced. A preferred runtime does not grant permission to
            request it.
          </p>
        </>
      )}
    </ReadState>
  );
}
