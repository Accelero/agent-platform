import { useId, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { api, type CatalogCollection, type VersionRef } from "../api";
import { useActorKey } from "../app/state";
import { ErrorNotice } from "./data";

/** Published catalog versions of one entry, newest first. */
export function usePublishedVersions(
  collection: CatalogCollection,
  id: string | undefined,
) {
  const actor = useActorKey();
  return useQuery({
    queryKey: [actor, "published", collection, id],
    enabled: !!id && /^[a-z]([-a-z0-9]*[a-z0-9])?$/.test(id),
    queryFn: async ({ signal }) =>
      (await api.versions(collection, id!, undefined, signal)).items
        .filter((version) => version.state === "published")
        .map((version) => version.version),
    staleTime: 15_000,
  });
}
/** Picks one published `{id, version}`; only published versions can be newly selected. */
export function VersionPicker({
  collection,
  label,
  value,
  onChange,
}: {
  collection: CatalogCollection;
  label: string;
  value?: VersionRef;
  onChange: (value: VersionRef | undefined) => void;
}) {
  const actor = useActorKey();
  const unique = useId();
  const [text, setText] = useState(value?.id ?? "");
  const entries = useQuery({
    queryKey: [actor, "catalog-picker", collection, text],
    queryFn: ({ signal }) =>
      api.catalog(collection, undefined, signal, text || undefined),
    staleTime: 15_000,
  });
  const versions = usePublishedVersions(collection, text);
  const published = versions.data ?? [];
  return (
    <fieldset>
      <legend>{label}</legend>
      <div className="field">
        <label htmlFor={`${unique}-id`}>{label}: ID</label>
        <input
          id={`${unique}-id`}
          list={`${unique}-entries`}
          maxLength={63}
          value={text}
          onChange={(event) => {
            setText(event.target.value);
            onChange(undefined);
          }}
        />
        <datalist id={`${unique}-entries`}>
          {entries.data?.items
            .filter((entry) => entry.latestPublished !== null)
            .map((entry) => (
              <option key={entry.id} value={entry.id} />
            ))}
        </datalist>
      </div>
      <div className="field">
        <label htmlFor={`${unique}-version`}>{label}: published version</label>
        <select
          id={`${unique}-version`}
          required
          value={value?.id === text && value ? String(value.version) : ""}
          disabled={!published.length}
          onChange={(event) =>
            onChange(
              event.target.value
                ? { id: text, version: Number(event.target.value) }
                : undefined,
            )
          }
        >
          <option value="">
            {published.length ? "Choose a version" : "No published version"}
          </option>
          {published.map((version) => (
            <option key={version} value={version}>
              v{version}
            </option>
          ))}
        </select>
      </div>
      {(entries.error || versions.error) && (
        <ErrorNotice error={(entries.error ?? versions.error)!} />
      )}
    </fieldset>
  );
}
