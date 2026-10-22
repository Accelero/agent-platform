import { OperationDetail } from "../features/operations/page";
import { useQuery } from "@tanstack/react-query";
import { useParams } from "@tanstack/react-router";
import {
  AdministratorsPage,
  AdministratorDetail,
} from "../features/administrators/page";
import { BatchRows, ResumeBatch } from "../features/assignments/rollout";
import { api } from "../api";
import { useActorKey } from "./state";
const TemplateEditor = lazy(() => import("../features/templates/editor"));
import { lazy, Suspense, useState } from "react";

import {
  createRootRoute,
  createRoute,
  createRouter,
} from "@tanstack/react-router";
import { Layout } from "./layout";
import { Overview } from "../features/overview/page";
import {
  AssignmentDetail,
  AssignmentsPage,
} from "../features/assignments/page";
import {
  RuntimeDetail,
  RuntimesPage,
} from "../features/terminal-runtimes/page";
import { ActivityPage, ActivityDetail } from "../features/activity/page";
import { UsersPage, UserDetail } from "../features/users/page";
import { TemplateDetail, TemplatesPage } from "../features/templates/page";
import { StorageDetail, StoragePage } from "../features/storage-bindings/page";
import { pollInterval } from "../components/command";
import { ReadState } from "../components/data";
import { validatePage } from "../components/data";
const root = createRootRoute({
  component: Layout,
  notFoundComponent: () => <p role="alert">This page was not found.</p>,
});
function BatchPage() {
  const { id } = useParams({ strict: false });
  const actor = useActorKey();
  const [started, setStarted] = useState(Date.now);
  const query = useQuery({
    queryKey: [actor, "template-batch", id],
    queryFn: ({ signal }) => api.templateBatch(id!, signal),
    refetchInterval: (query) =>
      pollInterval(
        started,
        Date.now(),
        query.state.dataUpdateCount,
        query.state.data?.state,
      ),
  });
  return (
    <>
      <p>
        Durable dispatch results. Each accepted instance operation reconciles
        independently.
      </p>
      <ReadState pending={query.isPending} error={query.error}>
        <BatchRows value={query.data} />
        {query.data && (
          <ResumeBatch
            value={query.data}
            onSuccess={() => {
              setStarted(Date.now());
              void query.refetch();
            }}
          />
        )}
      </ReadState>
    </>
  );
}
const routes = [
  createRoute({
    getParentRoute: () => root,
    path: "/users/$id",
    component: UserDetail,
  }),
  createRoute({
    getParentRoute: () => root,
    path: "/operations/$id",
    component: OperationDetail,
  }),
  createRoute({
    getParentRoute: () => root,
    path: "/activity/$id",
    component: ActivityDetail,
  }),
  createRoute({
    getParentRoute: () => root,
    path: "/administrators",
    validateSearch: validatePage,
    component: AdministratorsPage,
  }),
  createRoute({
    getParentRoute: () => root,
    path: "/administrators/$id",
    component: AdministratorDetail,
  }),
  createRoute({
    getParentRoute: () => root,
    path: "/template-batches/$id",
    component: BatchPage,
  }),
  createRoute({
    getParentRoute: () => root,
    path: "/templates/new",
    validateSearch: validatePage,
    component: () => (
      <Suspense fallback={<p>Loading editor…</p>}>
        <TemplateEditor />
      </Suspense>
    ),
  }),
  createRoute({
    getParentRoute: () => root,
    path: "/template-drafts/$id",
    component: () => (
      <Suspense fallback={<p>Loading editor…</p>}>
        <TemplateEditor />
      </Suspense>
    ),
  }),

  createRoute({
    getParentRoute: () => root,
    path: "/templates/$id",
    validateSearch: validatePage,
    component: TemplateDetail,
  }),
  createRoute({ getParentRoute: () => root, path: "/", component: Overview }),
  createRoute({
    getParentRoute: () => root,
    path: "/assignments",
    validateSearch: validatePage,
    component: AssignmentsPage,
  }),
  createRoute({
    getParentRoute: () => root,
    path: "/assignments/$id",
    validateSearch: validatePage,
    component: AssignmentDetail,
  }),
  createRoute({
    getParentRoute: () => root,
    path: "/terminal-runtimes",
    validateSearch: validatePage,
    component: RuntimesPage,
  }),
  createRoute({
    getParentRoute: () => root,
    path: "/terminal-runtimes/$id",
    validateSearch: validatePage,
    component: RuntimeDetail,
  }),
  createRoute({
    getParentRoute: () => root,
    path: "/activity",
    validateSearch: validatePage,
    component: ActivityPage,
  }),
  createRoute({
    getParentRoute: () => root,
    path: "/users",
    validateSearch: validatePage,
    component: UsersPage,
  }),
  createRoute({
    getParentRoute: () => root,
    path: "/templates",
    validateSearch: validatePage,
    component: TemplatesPage,
  }),
  createRoute({
    getParentRoute: () => root,
    path: "/storage-bindings",
    validateSearch: validatePage,
    component: StoragePage,
  }),
  createRoute({
    getParentRoute: () => root,
    path: "/storage-bindings/$id",
    component: StorageDetail,
  }),
];
export const router = createRouter({
  routeTree: root.addChildren(routes),
  defaultPreload: false,
});
declare module "@tanstack/react-router" {
  interface Register {
    router: typeof router;
  }
}
