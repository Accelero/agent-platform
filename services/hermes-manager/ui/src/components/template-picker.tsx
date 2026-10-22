import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { api } from "../api";
import { useActorKey } from "../app/state";
import { ReadState } from "./data";
export function TemplatePicker({
  value,
  onChange,
  label = "Approved template version",
}: {
  value: string;
  onChange: (v: string) => void;
  label?: string;
}) {
  const actor = useActorKey();
  const [q, setQ] = useState("");
  const [cursors, setCursors] = useState<string[]>([]);
  const query = useQuery({
    queryKey: [actor, "template-picker", q, cursors.at(-1)],
    queryFn: ({ signal }) => api.templates(signal, cursors.at(-1), { q }),
  });
  return (
    <fieldset>
      <legend>Choose a pinned template version</legend>
      <label>
        Search approved template versions
        <input
          value={q}
          maxLength={128}
          onChange={(e) => {
            setQ(e.target.value);
            setCursors([]);
          }}
        />
      </label>
      <ReadState pending={query.isPending} error={query.error}>
        {query.data && (
          <>
            <label>
              {label}
              <select
                aria-label={label}
                value={value}
                required
                onChange={(e) => onChange(e.target.value)}
              >
                <option value="">Select an approved pinned version</option>
                {value &&
                  !query.data.items.some(
                    (t) => `${t.id}:${t.version}` === value,
                  ) && <option value={value}>Selected: {value}</option>}
                {query.data.items.map((t) => (
                  <option
                    key={`${t.id}:${t.version}`}
                    value={`${t.id}:${t.version}`}
                  >
                    {t.id} · v{t.version}
                  </option>
                ))}
              </select>
            </label>
            <div className="pager">
              <button
                type="button"
                disabled={!cursors.length}
                onClick={() => setCursors([])}
              >
                First template versions
              </button>
              <button
                type="button"
                disabled={!cursors.length}
                onClick={() => setCursors((v) => v.slice(0, -1))}
              >
                Previous template versions
              </button>
              <button
                type="button"
                disabled={!query.data.nextCursor}
                onClick={() =>
                  setCursors((v) => [...v, query.data!.nextCursor!])
                }
              >
                More template versions
              </button>
            </div>
          </>
        )}
      </ReadState>
    </fieldset>
  );
}
