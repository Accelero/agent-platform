import type { CatalogCollection } from "../../api";
import type { Draft } from "./model";
/** A starting point for a new catalog entry; images must be pinned. */
export function initialDefinition(collection: CatalogCollection): Draft {
  return collection === "templates"
    ? {
        schemaVersion: 2,
        image: "",
        resources: { cpuMillis: 1000, memoryMib: 2048 },
        home: { source: "", sizeGib: 5 },
        mounts: { workspace: { mountPath: "/workspace" } },
        defaultVolumes: { workspace: { source: "", sizeGib: 10 } },
        adminConfig: {},
        secrets: [],
      }
    : {
        image: "",
        sshContract: "managed-ssh-v2",
        uid: 1000,
        gid: 1000,
        resources: { cpuMillis: 2000, memoryMib: 4096, ephemeralGib: 10 },
        limits: { idleSeconds: 900, maxLeaseSeconds: 3600, maxEnvironments: 4 },
        mounts: { workspace: { mountPath: "/workspace" } },
      };
}
