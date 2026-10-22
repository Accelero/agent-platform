import { Command, type Field } from "../../components/command";
import { mountFields } from "../terminal-runtimes/commands";
import type { components } from "../../generated/api";
export function StorageCommand({
  instanceId,
  binding,
  etag,
}: {
  instanceId?: string;
  binding?: components["schemas"]["StorageBinding"];
  etag?: string | null;
}) {
  const fields: Field[] = [
    { key: "id", label: "Binding ID", readOnly: !!binding },
    {
      key: "version",
      label: "Binding version",
      type: "number",
      min: 1,
      readOnly: true,
    },
    {
      key: "instanceId",
      label: "Instance ID",
      readOnly: !!binding || !!instanceId,
    },
    { key: "namespace", label: "Namespace", readOnly: !!binding },
    { key: "namespaceUid", label: "Namespace UID", readOnly: !!binding },
    { key: "pvcName", label: "PVC name", readOnly: !!binding },
    { key: "pvcUid", label: "PVC UID", readOnly: !!binding },
    {
      key: "backingStorageId",
      label: "Backing storage inventory ID",
      readOnly: !!binding,
    },
    { key: "external", label: "External storage", type: "checkbox" },
    { key: "required", label: "Required", type: "checkbox" },
    {
      key: "backup",
      label: "Backup policy",
      options: ["platform_checkpoint", "external", "excluded"],
    },
    {
      key: "mounts",
      label: "Permitted mount",
      max: 16,
      item: { key: "", label: "", fields: mountFields },
      initial: { path: "", readOnly: true },
    },
  ];
  return (
    <Command
      title={binding ? "Update storage binding" : "Register storage binding"}
      permission={
        binding ? "execution.storage.update" : "execution.storage.register"
      }
      method={binding ? "PATCH" : "POST"}
      path={
        binding
          ? `/api/v1/storage-bindings/${binding.id}`
          : "/api/v1/storage-bindings"
      }
      etag={binding ? etag : undefined}
      current={binding}
      initial={
        binding
          ? { ...binding, version: binding.version + 1 }
          : {
              id: crypto.randomUUID(),
              version: 1,
              instanceId: instanceId ?? "",
              namespace: "",
              namespaceUid: "",
              pvcName: "",
              pvcUid: "",
              backingStorageId: "",
              external: false,
              required: true,
              backup: "platform_checkpoint",
              mounts: [{ path: "", readOnly: true }],
            }
      }
      fields={fields}
    />
  );
}
