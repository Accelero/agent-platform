import {
  createRootRoute,
  createRoute,
  createRouter,
} from "@tanstack/react-router";
import { Layout } from "./layout";
import { validatePage } from "../components/data";
import { Overview } from "../features/overview/page";
import { UsersPage, UserDetail } from "../features/users/page";
import {
  AssignmentDetail,
  AssignmentsPage,
} from "../features/assignments/page";
import { CatalogDetail, CatalogPage } from "../features/catalog/page";
import { StoragePage } from "../features/storage/page";
import { SecretsPage } from "../features/secrets/page";
import { ActivityDetail, ActivityPage } from "../features/activity/page";
import {
  AdministratorDetail,
  AdministratorsPage,
} from "../features/administrators/page";

// Every path here must also be listed in `hermes_http::admin::assets::PAGES`.
const root = createRootRoute({
  component: Layout,
  notFoundComponent: () => <p role="alert">This page was not found.</p>,
});
const routes = [
  createRoute({
    getParentRoute: () => root,
    path: "/",
    component: Overview,
  }),
  createRoute({
    getParentRoute: () => root,
    path: "/users",
    validateSearch: validatePage,
    component: UsersPage,
  }),
  createRoute({
    getParentRoute: () => root,
    path: "/users/$id",
    component: UserDetail,
  }),
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
    path: "/templates",
    validateSearch: validatePage,
    component: () => <CatalogPage collection="templates" />,
  }),
  createRoute({
    getParentRoute: () => root,
    path: "/templates/$id",
    validateSearch: validatePage,
    component: () => <CatalogDetail collection="templates" />,
  }),
  createRoute({
    getParentRoute: () => root,
    path: "/terminal-backends",
    validateSearch: validatePage,
    component: () => <CatalogPage collection="terminal-backends" />,
  }),
  createRoute({
    getParentRoute: () => root,
    path: "/terminal-backends/$id",
    validateSearch: validatePage,
    component: () => <CatalogDetail collection="terminal-backends" />,
  }),
  createRoute({
    getParentRoute: () => root,
    path: "/storage",
    validateSearch: validatePage,
    component: StoragePage,
  }),
  createRoute({
    getParentRoute: () => root,
    path: "/secrets",
    validateSearch: validatePage,
    component: SecretsPage,
  }),
  createRoute({
    getParentRoute: () => root,
    path: "/activity",
    validateSearch: validatePage,
    component: ActivityPage,
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
