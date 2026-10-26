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
  ["/users", "Users"],
  ["/assignments", "Assignments"],
  ["/templates", "Hermes templates"],
  ["/terminal-backends", "Terminal backends"],
  ["/storage", "Storage"],
  ["/secrets", "Secrets"],
  ["/activity", "Activity"],
  ["/administrators", "Administrators"],
] as const;
const titles: Record<string, [string, string]> = {
  "/": [
    "A place for every agent.",
    "Manage personal Hermes instances and the terminal backends they may use.",
  ],
  "/users": [
    "Users",
    "Exact identities known to the manager, and the optional Keycloak directory.",
  ],
  "/assignments": [
    "Assignments",
    "Who gets a Hermes instance: template version, volumes, terminal backends and admin settings.",
  ],
  "/templates": [
    "Hermes templates",
    "Versioned instance definitions with their enforced admin settings.",
  ],
  "/terminal-backends": [
    "Terminal backends",
    "Versioned SSH worker definitions that assignments may allow.",
  ],
  "/storage": [
    "Storage",
    "Approved storage sources from the installation and the volumes on them.",
  ],
  "/secrets": [
    "Admin secrets",
    "Write-only values sealed to the controller. Values are never shown.",
  ],
  "/activity": ["Activity", "An attributed record of every committed change."],
  "/administrators": [
    "Administrators",
    "PostgreSQL grants for exact Keycloak identities.",
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
