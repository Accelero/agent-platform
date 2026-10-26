import { useState } from "react";
import { useMutation } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { createIntent, type Result, type VersionRef } from "../../api";
import { queryClient, useSession } from "../../app/state";
import { VersionPicker } from "../../components/catalog-picker";
import { usePermitted } from "../../components/command";
import { ErrorNotice, StatusBadge } from "../../components/data";

type RolloutResult = Result<"rolloutTemplate">;
/**
 * Applies one reviewed template version to explicitly selected assignments.
 * Each member is its own change; the result reports each one.
 */
export function Rollout({
  selected,
  onDone,
}: {
  selected: Map<string, number>;
  onDone: () => void;
}) {
  const session = useSession();
  const permitted = usePermitted("administer");
  const [template, setTemplate] = useState<VersionRef>();
  const [submit] = useState(() => createIntent());
  const mutation = useMutation({
    mutationFn: () =>
      submit<RolloutResult>(
        "POST",
        "/api/v1/assignment-rollouts",
        {
          template,
          items: [...selected].map(([assignmentId, expectedVersion]) => ({
            assignmentId,
            expectedVersion,
          })),
        },
        session.data!.csrfToken!,
      ),
    onSuccess: () => {
      void queryClient.invalidateQueries();
      onDone();
    },
  });
  if (!permitted) return null;
  return (
    <section className="panel detail" aria-label="Set template">
      <h2>Set template for selected assignments</h2>
      <p>
        {selected.size} selected (at most 50). Publishing never changes
        assignments; this applies one exact version to the selected ones.
      </p>
      <form
        onSubmit={(event) => {
          event.preventDefault();
          mutation.mutate();
        }}
      >
        <fieldset disabled={mutation.isPending}>
          <VersionPicker
            collection="templates"
            label="Template version"
            value={template}
            onChange={setTemplate}
          />
        </fieldset>
        <button
          disabled={
            !template ||
            !selected.size ||
            mutation.isPending ||
            !session.data?.csrfToken
          }
        >
          Apply to {selected.size} assignments
        </button>
      </form>
      {mutation.error && <ErrorNotice error={mutation.error} />}
      {mutation.data && (
        <table>
          <caption>Rollout results</caption>
          <thead>
            <tr>
              <th scope="col">Assignment</th>
              <th scope="col">Outcome</th>
              <th scope="col">Detail</th>
            </tr>
          </thead>
          <tbody>
            {mutation.data.results.map((result) => (
              <tr key={result.assignmentId}>
                <td>
                  <Link
                    to="/assignments/$id"
                    params={{ id: result.assignmentId }}
                  >
                    {result.assignmentId}
                  </Link>
                </td>
                <td>
                  <StatusBadge value={result.outcome} />
                </td>
                <td>
                  {result.receipt
                    ? `Now version ${result.receipt.version}`
                    : result.error?.error}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </section>
  );
}
