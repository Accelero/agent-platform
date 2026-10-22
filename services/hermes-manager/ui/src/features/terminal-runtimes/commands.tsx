import { Command, type Field } from "../../components/command";
export const resourcesFields: Field[] = [
  {
    key: "cpuMillis",
    label: "CPU (millicores)",
    type: "number",
    min: 10,
    max: 64000,
  },
  {
    key: "memoryMib",
    label: "Memory (MiB)",
    type: "number",
    min: 64,
    max: 262144,
  },
  {
    key: "ephemeralStorageMib",
    label: "Ephemeral storage (MiB)",
    type: "number",
    min: 64,
    max: 1048576,
  },
];
export const resourcesInitial = {
  cpuMillis: 1000,
  memoryMib: 1024,
  ephemeralStorageMib: 1024,
};
export const limitsFields: Field[] = [
  {
    key: "ttlSeconds",
    label: "TTL seconds",
    type: "number",
    min: 30,
    max: 600,
  },
  { key: "exclusive", label: "Exclusive", type: "checkbox" },
  { key: "serializeProfile", label: "Serialize profile", type: "checkbox" },
  {
    key: "maxEnvironments",
    label: "Maximum environments",
    type: "number",
    min: 1,
    max: 64,
  },
  {
    key: "maxLeases",
    label: "Maximum leases",
    type: "number",
    min: 1,
    max: 128,
  },
  {
    key: "idleSeconds",
    label: "Idle seconds",
    type: "number",
    min: 30,
    max: 86400,
  },
];
export const limitsInitial = {
  ttlSeconds: 120,
  exclusive: false,
  serializeProfile: false,
  maxEnvironments: 8,
  maxLeases: 32,
  idleSeconds: 900,
};
export const mountFields: Field[] = [
  { key: "path", label: "Approved mount path" },
  { key: "readOnly", label: "Read only", type: "checkbox" },
  { key: "subPath", label: "Subpath", optional: true },
];
export function RegisterRuntime({
  id,
  definition,
  nextVersion,
}: {
  id?: string;
  definition?: import("../../generated/api").components["schemas"]["RuntimeDefinition"];
  nextVersion?: number;
}) {
  return (
    <Command
      title={
        definition
          ? `Register runtime version from v${definition.runtime.version}`
          : id
            ? "Register runtime version"
            : "Register runtime"
      }
      permission="execution.runtime.register"
      method="POST"
      path={
        id
          ? `/api/v1/terminal-runtimes/${encodeURIComponent(id)}/versions`
          : "/api/v1/terminal-runtimes"
      }
      initial={
        definition
          ? {
              ...definition,
              runtime: {
                ...definition.runtime,
                version: nextVersion ?? definition.runtime.version + 1,
              },
            }
          : {
              schemaVersion: 1,
              runtime: { id: id ?? "", version: nextVersion ?? 1 },
              image: "",
              sshContract: "managed-ssh-v2",
              uid: 10001,
              gid: 10001,
              serviceAccount: "worker",
              resources: resourcesInitial,
              locale: "C.UTF-8",
              egress: [],
              pullSecretNames: [],
            }
      }
      fields={[
        {
          key: "runtime",
          label: "Runtime",
          fields: [
            { key: "id", label: "Runtime ID", readOnly: !!id },
            { key: "version", label: "Version", type: "number", min: 1 },
          ],
        },
        { key: "image", label: "Image pinned by digest" },
        {
          key: "sshContract",
          label: "SSH contract",
          options: ["managed-ssh-v1", "managed-ssh-v2"],
        },
        { key: "uid", label: "User ID", type: "number", min: 1, max: 65535 },
        { key: "gid", label: "Group ID", type: "number", min: 1, max: 65535 },
        { key: "resources", label: "Resources", fields: resourcesFields },
        { key: "locale", label: "Locale", options: ["C.UTF-8", "en_US.UTF-8"] },
        {
          key: "egress",
          label: "Service egress",
          item: {
            key: "",
            label: "",
            fields: [
              { key: "namespace", label: "Namespace" },
              { key: "service", label: "Service" },
              {
                key: "port",
                label: "Port",
                type: "number",
                min: 1,
                max: 65535,
              },
            ],
          },
          initial: { namespace: "", service: "", port: 443 },
        },
        {
          key: "pullSecretNames",
          label: "Pull secret name",
          max: 4,
          item: { key: "", label: "" },
          initial: "",
        },
      ]}
    />
  );
}
