import { StrictMode, useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import type { ReactNode } from "react";
import { api, ApiError } from "./api";
import type { AdminSession, Assignments, Runtimes, Audit } from "./api";
import "./style.css";

const navigation = [
  ["/", "Overview", "01"],
  ["/assignments", "Instances", "02"],
  ["/terminal-runtimes", "Terminal backends", "03"],
  ["/activity", "Activity", "04"],
];
const titles: Record<string, [string, string]> = {
  "/": [
    "A place for every agent.",
    "Manage personal Hermes instances and their approved execution environments.",
  ],
  "/assignments": [
    "Personal instances",
    "Assignments connect an exact Keycloak identity to its own Hermes instance.",
  ],
  "/terminal-runtimes": [
    "Terminal backends",
    "The approved runtime catalog available to assignment policies.",
  ],
  "/activity": [
    "Manager activity",
    "An administrator-attributed record of committed changes.",
  ],
};

function App() {
  const path = window.location.pathname;
  const [session, setSession] = useState<AdminSession>();
  const [assignments, setAssignments] = useState<Assignments>();
  const [runtimes, setRuntimes] = useState<Runtimes>();
  const [audit, setAudit] = useState<Audit>();
  const [error, setError] = useState<Error>();
  const [loading, setLoading] = useState(true);
  const [refresh, setRefresh] = useState(0);
  const [signingOut, setSigningOut] = useState(false);
  useEffect(() => {
    let active = true;
    setLoading(true);
    setError(undefined);
    setSession(undefined);
    setAssignments(undefined);
    setRuntimes(undefined);
    setAudit(undefined);
    (async () => {
      try {
        const actor = await api.session();
        if (!active) return;
        setSession(actor);
        if (path === "/assignments") {
          const rows = await api.assignments();
          if (active) setAssignments(rows);
        } else if (path === "/terminal-runtimes") {
          const rows = await api.runtimes();
          if (active) setRuntimes(rows);
        } else if (path === "/activity") {
          const rows = await api.audit();
          if (active) setAudit(rows);
        }
      } catch (cause) {
        if (active) {
          setError(
            cause instanceof Error
              ? cause
              : new Error("Unable to reach the manager."),
          );
          if (
            cause instanceof ApiError &&
            (cause.status === 401 || cause.status === 403)
          ) {
            setSession(undefined);
            setAssignments(undefined);
            setRuntimes(undefined);
            setAudit(undefined);
          }
        }
      } finally {
        if (active) setLoading(false);
      }
    })();
    return () => {
      active = false;
    };
  }, [path, refresh]);
  async function logout() {
    if (!session?.csrfToken) return;
    setSigningOut(true);
    try {
      await api.logout(session.csrfToken);
      setSession(undefined);
      setAssignments(undefined);
      setRuntimes(undefined);
      setAudit(undefined);
      window.location.replace("/auth/signed-out");
    } catch (cause) {
      setError(cause instanceof Error ? cause : new Error("Sign out failed."));
      setSigningOut(false);
    }
  }
  const [title, description] = titles[path] ?? titles["/"];
  return (
    <div className="app">
      <aside className="sidebar">
        <a className="brand" href="/" aria-label="Hermes Manager home">
          <span className="brand-icon">h.</span>
          <span>
            hermes<span className="brand-caption">MANAGER</span>
          </span>
        </a>
        <div className="nav-caption">WORKSPACE</div>
        <nav aria-label="Main navigation">
          {navigation.map(([url, label, number]) => (
            <a
              key={url}
              href={url}
              aria-current={path === url ? "page" : undefined}
            >
              <span>{number}</span>
              {label}
              <span className="nav-arrow">↗</span>
            </a>
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
            <button onClick={() => setRefresh((r) => r + 1)} disabled={loading}>
              ↻ Refresh
            </button>
          </div>
          {error && (
            <div role="alert" className="error">
              {error.message}{" "}
              {error instanceof ApiError && error.status === 401 && (
                <a href="/auth/login">Sign in with Keycloak →</a>
              )}
            </div>
          )}
          {loading && (
            <p role="status" className="loading">
              Connecting to the manager…
            </p>
          )}
          {session && (
            <>
              {path === "/" && (
                <>
                  <section className="welcome">
                    <div>
                      <span className="label">CONNECTED TO YOUR MANAGER</span>
                      <h2>Infrastructure, with identity.</h2>
                      <p>
                        Each user gets their own Hermes. Each agent gets only
                        the execution environments you allow.
                      </p>
                      <a href="/assignments" className="primary-link">
                        Explore instances <span>→</span>
                      </a>
                    </div>
                    <div className="orbit" aria-hidden="true">
                      <span>h.</span>
                      <i />
                      <b>SSH</b>
                      <em>OIDC</em>
                    </div>
                  </section>
                  <section className="cards" aria-label="Management areas">
                    <Card
                      number="01"
                      title="Personal instances"
                      href="/assignments"
                    >
                      Review user assignments, selected templates and observed
                      lifecycle state.
                    </Card>
                    <Card
                      number="02"
                      title="Approved execution"
                      href="/terminal-runtimes"
                    >
                      Inspect versioned terminal types before granting access to
                      an instance.
                    </Card>
                    <Card
                      number="03"
                      title="Accountable changes"
                      href="/activity"
                    >
                      Follow operations through the manager’s administrator
                      audit trail.
                    </Card>
                  </section>
                  <section className="scope-note">
                    <span className="small-dot" />
                    <div>
                      <strong>Your control plane is connected</strong>
                      <p>
                        This initial dashboard provides read-only lists.
                        Assignment and policy editors are planned.
                      </p>
                    </div>
                    <span className="pill">SCAFFOLD</span>
                  </section>
                </>
              )}
              {assignments && (
                <section className="panel">
                  <div className="panel-heading">
                    <h2>Instance assignments</h2>
                    <span>{assignments.items.length} on this page</span>
                  </div>
                  {assignments.items.length ? (
                    <div className="table-scroll">
                      <table>
                        <thead>
                          <tr>
                            <th>Owner subject</th>
                            <th>Template</th>
                            <th>Access</th>
                            <th>State</th>
                            <th>Generation</th>
                          </tr>
                        </thead>
                        <tbody>
                          {assignments.items.map((item) => (
                            <tr key={item.id}>
                              <td>
                                <strong>{item.identity.subject}</strong>
                                <small>{item.identity.issuer}</small>
                              </td>
                              <td>
                                {item.template.id} · v{item.template.version}
                              </td>
                              <td>{item.access}</td>
                              <td>
                                <span className="pill">{item.state}</span>
                              </td>
                              <td>
                                {item.observedGeneration} /{" "}
                                {item.desiredGeneration}
                              </td>
                            </tr>
                          ))}
                        </tbody>
                      </table>
                    </div>
                  ) : (
                    <Empty>No assignments are registered yet.</Empty>
                  )}
                  {assignments.nextCursor && (
                    <p className="page-note">
                      Showing the first 50 entries. Pagination controls are
                      planned.
                    </p>
                  )}
                </section>
              )}
              {runtimes && (
                <section className="panel">
                  <div className="panel-heading">
                    <h2>Approved terminal types</h2>
                    <span>{runtimes.items.length} on this page</span>
                  </div>
                  {runtimes.items.length ? (
                    <div className="table-scroll">
                      <table>
                        <thead>
                          <tr>
                            <th>Runtime ID</th>
                            <th>Version</th>
                            <th>Definition</th>
                          </tr>
                        </thead>
                        <tbody>
                          {runtimes.items.map((item) => (
                            <tr key={item.runtime.id}>
                              <td>
                                <strong>{item.runtime.id}</strong>
                              </td>
                              <td>{item.runtime.version}</td>
                              <td className="mono">{item.image}</td>
                            </tr>
                          ))}
                        </tbody>
                      </table>
                    </div>
                  ) : (
                    <Empty>No terminal backends are registered yet.</Empty>
                  )}
                  {runtimes.nextCursor && (
                    <p className="page-note">
                      Showing the first 50 entries. Pagination controls are
                      planned.
                    </p>
                  )}
                </section>
              )}
              {audit && (
                <section className="panel">
                  <div className="panel-heading">
                    <h2>Audit events</h2>
                    <span>{audit.items.length} on this page</span>
                  </div>
                  {audit.items.length ? (
                    <div className="table-scroll">
                      <table>
                        <thead>
                          <tr>
                            <th>Action</th>
                            <th>Administrator</th>
                            <th>Assignment</th>
                            <th>Time</th>
                          </tr>
                        </thead>
                        <tbody>
                          {audit.items.map((item) => (
                            <tr key={item.id}>
                              <td>
                                <strong>{item.action}</strong>
                              </td>
                              <td>{item.actor.subject}</td>
                              <td className="mono">{item.assignmentId}</td>
                              <td>
                                {new Date(
                                  item.occurredAt * 1000,
                                ).toLocaleString()}
                              </td>
                            </tr>
                          ))}
                        </tbody>
                      </table>
                    </div>
                  ) : (
                    <Empty>
                      No administrator changes have been recorded yet.
                    </Empty>
                  )}
                  {audit.nextCursor && (
                    <p className="page-note">
                      Showing the first 50 entries. Pagination controls are
                      planned.
                    </p>
                  )}
                </section>
              )}
              <section className="identity">
                <div>
                  <span className="label">SIGNED IN WITH KEYCLOAK</span>
                  <strong>{session.actor.subject}</strong>
                  <span>{session.actor.issuer}</span>
                </div>
                <button
                  disabled={!session.csrfToken || signingOut}
                  onClick={logout}
                >
                  {signingOut ? "Signing out…" : "Sign out ↗"}
                </button>
              </section>
            </>
          )}
          <footer>
            <span>HERMES MANAGER</span>
            <span>Private instances. Explicit permissions.</span>
          </footer>
        </main>
      </div>
    </div>
  );
}

function Card({
  number,
  title,
  href,
  children,
}: {
  number: string;
  title: string;
  href: string;
  children: ReactNode;
}) {
  return (
    <a className="card" href={href}>
      <span className="card-number">
        {number}
        <span>↗</span>
      </span>
      <h2>{title}</h2>
      <p>{children}</p>
    </a>
  );
}
function Empty({ children }: { children: ReactNode }) {
  return (
    <div className="empty">
      <span>↳</span>
      <p>{children}</p>
    </div>
  );
}

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
