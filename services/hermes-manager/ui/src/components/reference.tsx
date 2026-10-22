import { useId } from "react";
import { useQuery } from "@tanstack/react-query";
import { api } from "../api";
import { queryClient, useActorKey } from "../app/state";
import { ErrorNotice } from "./data";
export type ReferenceKind = "runtime" | "storageClass" | "binding";
export type ReferenceChoice = { id: string; label: string; version?: number };
export async function references(
  kind: ReferenceKind,
  q: string,
  signal: AbortSignal,
  instanceId?: string,
): Promise<ReferenceChoice[]> {
  if (kind === "storageClass")
    return (await api.storageClasses(q, signal)).items.map((v) => ({
      id: v.id,
      label: `${v.id} · approved template reference`,
    }));
  if (kind === "binding")
    return instanceId
      ? (await api.bindingReferences(instanceId, q, signal)).items.map((v) => ({
          id: v.id,
          version: v.version,
          label: `${v.id} · v${v.version} · ${v.pvcName}`,
        }))
      : [];
  return (await api.runtimes(undefined, signal, { q })).items.map((v) => ({
    id: v.runtime.id,
    version: v.runtime.version,
    label: `${v.runtime.id} · v${v.runtime.version}`,
  }));
}
export function lookupReferences(
  actor: string | undefined,
  kind: ReferenceKind,
  q: string,
  instanceId?: string,
) {
  return queryClient.fetchQuery({
    queryKey: [actor, "reference", kind, instanceId, q],
    queryFn: ({ signal }) => references(kind, q, signal, instanceId),
    staleTime: 30_000,
  });
}
export function ReferenceInput({
  kind,
  value,
  onChange,
  onSelect,
  instanceId,
  id,
  readOnly,
  required = true,
}: {
  kind: ReferenceKind;
  value: string;
  onChange: (v: string) => void;
  onSelect?: (v: ReferenceChoice) => void;
  instanceId?: string;
  id?: string;
  readOnly?: boolean;
  required?: boolean;
}) {
  const actor = useActorKey();
  const list = useId();
  const query = useQuery({
    queryKey: [actor, "reference", kind, instanceId, value],
    enabled: !readOnly && (kind !== "binding" || !!instanceId),
    queryFn: ({ signal }) => references(kind, value, signal, instanceId),
    staleTime: 30_000,
  });
  return (
    <>
      <input
        id={id}
        list={list}
        value={value}
        required={required}
        readOnly={readOnly}
        maxLength={128}
        onChange={(e) => {
          onChange(e.target.value);
          const selected = query.data?.find((v) => v.id === e.target.value);
          if (selected) onSelect?.(selected);
        }}
      />
      <datalist id={list}>
        {query.data?.map((v) => (
          <option key={v.id} value={v.id}>
            {v.label}
          </option>
        ))}
      </datalist>
      {query.error && <ErrorNotice error={query.error} />}
      <small>
        Search registered references.{" "}
        {kind === "storageClass"
          ? "Choices come from approved templates; Kubernetes availability is checked during reconciliation."
          : "Choosing a reference does not grant access."}
      </small>
    </>
  );
}
