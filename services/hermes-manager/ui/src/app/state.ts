import {
  MutationCache,
  QueryCache,
  QueryClient,
  useQuery,
} from "@tanstack/react-query";
import { useSyncExternalStore } from "react";
import { ApiError, api } from "../api";
export function retryRead(count: number, error: Error) {
  return (
    count < 2 &&
    (error instanceof TypeError ||
      (error instanceof ApiError && error.status === 503))
  );
}
let lost: ApiError | null = null;
let actor: string | undefined;
const listeners = new Set<() => void>();
function emit() {
  for (const notify of listeners) notify();
}
const draftResets = new Set<() => void>();
export function registerPrivateReset(reset: () => void) {
  draftResets.add(reset);
  return () => {
    draftResets.delete(reset);
  };
}
export function clearPrivateState(client: QueryClient = queryClient) {
  client.clear();
  for (const reset of draftResets) reset();
  actor = undefined;
}
function onError(error: Error) {
  if (error instanceof ApiError && [401, 403].includes(error.status)) {
    lost = error;
    clearPrivateState();
    emit();
    if (error.status === 401)
      window.location.replace(
        `/auth/login?returnTo=${encodeURIComponent(window.location.pathname + window.location.search)}`,
      );
  }
}
export const queryClient = new QueryClient({
  queryCache: new QueryCache({ onError }),
  mutationCache: new MutationCache({ onError }),
  defaultOptions: {
    queries: {
      retry: retryRead,
      retryDelay: (count) => Math.min(500 * 2 ** count, 2000),
      refetchOnWindowFocus: false,
    },
    mutations: { retry: false },
  },
});
export function useAuthorizationError() {
  return useSyncExternalStore(
    (notify) => {
      listeners.add(notify);
      return () => {
        listeners.delete(notify);
      };
    },
    () => lost,
  );
}
export function useSession() {
  return useQuery({
    queryKey: ["session"],
    staleTime: 60_000,
    refetchInterval: 60_000,
    enabled: !lost,
    queryFn: async ({ signal }) => {
      const session = await api.session(signal);
      const next = JSON.stringify(session.actor);
      if (actor && actor !== next) {
        clearPrivateState();
        window.location.reload();
        throw new Error("Administrator identity changed. Reloading.");
      }
      actor = next;
      return session;
    },
  });
}
export function useActorKey() {
  const { data } = useSession();
  return JSON.stringify(data?.actor);
}
