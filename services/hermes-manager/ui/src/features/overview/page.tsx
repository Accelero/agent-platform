import type { ReactNode } from "react";
import { Link } from "@tanstack/react-router";
export function Overview() {
  return (
    <>
      <section className="welcome">
        <div>
          <span className="label">CONNECTED TO YOUR MANAGER</span>
          <h2>Infrastructure, with identity.</h2>
          <p>
            Each user gets their own Hermes. Each agent gets only the execution
            environments you allow.
          </p>
          <Link to="/assignments" className="primary-link">
            Explore instances <span>→</span>
          </Link>
        </div>
        <div className="orbit" aria-hidden="true">
          <span>h.</span>
          <i />
          <b>SSH</b>
          <em>OIDC</em>
        </div>
      </section>
      <section className="cards" aria-label="Management areas">
        <Card number="01" title="Personal instances" href="/assignments">
          Review user assignments, selected templates and observed lifecycle
          state.
        </Card>
        <Card number="02" title="Approved execution" href="/terminal-runtimes">
          Inspect versioned terminal types before granting access to an
          instance.
        </Card>
        <Card number="03" title="Accountable changes" href="/activity">
          Follow operations through the manager’s administrator audit trail.
        </Card>
      </section>
      <section className="scope-note">
        <span className="small-dot" />
        <div>
          <strong>Your control plane is connected</strong>
          <p>
            This initial dashboard provides read-only lists. Assignment and
            policy editors are planned.
          </p>
        </div>
        <span className="pill">CONNECTED</span>
      </section>
    </>
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
  href: "/assignments" | "/terminal-runtimes" | "/activity";
  children: ReactNode;
}) {
  return (
    <Link className="card" to={href}>
      <span className="card-number">
        {number}
        <span>↗</span>
      </span>
      <h2>{title}</h2>
      <p>{children}</p>
    </Link>
  );
}
