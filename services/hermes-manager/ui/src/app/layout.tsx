import { useState } from "react";
import { Link, Outlet, useLocation } from "@tanstack/react-router";
import { useMutation } from "@tanstack/react-query";
import { api } from "../api";
import {
  clearPrivateState,
  queryClient,
  useAuthorizationError,
  useSession,
} from "./state";
import { ErrorNotice } from "../components/data";
const navigation = [
  ["/", "Overview"],
  ["/assignments", "Instances"],
  ["/users", "Users"],
  ["/templates", "Templates"],
  ["/terminal-runtimes", "Terminal backends"],
  ["/storage-bindings", "Storage bindings"],
  ["/activity", "Activity"],
  ["/administrators", "Administrators"],
] as const;
const titles: Record<string, [string, string]> = {
  "/operations": [
    "Operation inspection",
    "Track reconciliation outcomes and verified journal steps.",
  ],
  "/administrators": [
    "Administrators",
    "Manage PostgreSQL administration grants for exact Keycloak identities.",
  ],
  "/template-drafts": [
    "Template draft",
    "Edit, validate and publish an immutable version.",
  ],
  "/template-batches": [
    "Bulk template application",
    "Inspect durable results for the explicitly selected assignments.",
  ],
  "/": [
    "A place for every agent.",
    "Manage personal Hermes instances and their approved execution environments.",
  ],
  "/assignments": [
    "Personal instances",
    "Assignments connect an exact Keycloak identity to its own Hermes instance.",
  ],
  "/users": [
    "Users",
    "Exact identities recorded by the manager and their directory verification.",
  ],
  "/templates": [
    "Template definitions",
    "Inspect deployment settings, managed configuration and immutable catalog versions.",
  ],
  "/terminal-runtimes": [
    "Terminal backends",
    "The approved runtime catalog available to assignment policies.",
  ],
  "/storage-bindings": [
    "Storage bindings",
    "Inspect approved storage for an exact instance.",
  ],
  "/activity": [
    "Manager activity",
    "An administrator-attributed record of committed changes.",
  ],
};
export function Layout() {
  const path = useLocation({ select: (location) => location.pathname });
  const session = useSession();
  const denied = useAuthorizationError();
  const [signingOut, setSigningOut] = useState(false);
  const logout = useMutation({
    mutationFn: async () => {
      const csrf = session.data?.csrfToken;
      if (!csrf) throw new Error("A browser session is required to sign out.");
      setSigningOut(true);
      clearPrivateState();
      await api.logout(csrf);
      window.location.replace("/auth/signed-out");
    },
    onError: () => setSigningOut(false),
  });
  const [title, description] = titles[`/${path.split("/")[1]}`] ?? titles["/"];
  const error = denied ?? session.error;
  return (
    <div className="app">
      <aside className="sidebar">
        <Link className="brand" to="/" aria-label="Hermes Manager home">
          <span className="brand-icon">h.</span>
          <span>
            hermes<span className="brand-caption">MANAGER</span>
          </span>
        </Link>
        <div className="nav-caption">WORKSPACE</div>
        <nav aria-label="Main navigation">
          {navigation.map(([url, label], i) => (
            <Link
              key={url}
              to={url}
              aria-current={
                path === url || (url !== "/" && path.startsWith(`${url}/`))
                  ? "page"
                  : undefined
              }
            >
              <span>{String(i + 1).padStart(2, "0")}</span>
              {label}
              <span className="nav-arrow">↗</span>
            </Link>
          ))}
        </nav>
        <div className="sidebar-bottom">
          <span className="small-dot" />
          Administration<span>Personal agent infrastructure</span>
        </div>
      </aside>
      <div className="workspace">
        <header>
          <span>
            Agent platform <span className="slash">/</span>{" "}
            <strong>Administration</strong>
          </span>
          <span className="admin-badge">ADMIN ACCESS</span>
        </header>
        <main id="main">
          <div className="eyebrow">HERMES CONTROL PLANE</div>
          <div className="page-heading">
            <div>
              <h1>{title}</h1>
              <p>{description}</p>
            </div>
            <button
              disabled={signingOut}
              onClick={() => void queryClient.invalidateQueries()}
            >
              ↻ Refresh
            </button>
          </div>
          {error ? (
            <ErrorNotice error={error} />
          ) : session.isPending ? (
            <p role="status" className="loading">
              Connecting to the manager…
            </p>
          ) : (
            !signingOut &&
            session.data && (
              <>
                <Outlet />
                <section className="identity">
                  <div>
                    <span className="label">SIGNED IN WITH KEYCLOAK</span>
                    <strong>{session.data.actor.subject}</strong>
                    <span>{session.data.actor.issuer}</span>
                  </div>
                  <button
                    disabled={!session.data.csrfToken || logout.isPending}
                    onClick={() => logout.mutate()}
                  >
                    Sign out ↗
                  </button>
                </section>
              </>
            )
          )}
          {logout.error && <ErrorNotice error={logout.error} />}
          <footer>
            <span>HERMES MANAGER</span>
            <span>Private instances. Explicit permissions.</span>
          </footer>
        </main>
      </div>
    </div>
  );
}
