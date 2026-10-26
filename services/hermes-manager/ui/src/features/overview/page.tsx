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
            Review assignments <span>→</span>
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
        <Card number="01" title="Who gets a Hermes" href="/assignments">
          Assign an exact identity a template version, volumes and the terminal
          backends it may use.
        </Card>
        <Card number="02" title="Approved execution" href="/terminal-backends">
          Publish versioned SSH worker definitions before allowing them.
        </Card>
        <Card number="03" title="Accountable changes" href="/activity">
          Every committed change is attributed and recorded.
        </Card>
      </section>
      <section className="scope-note">
        <span className="small-dot" />
        <div>
          <strong>Accepted is not yet applied</strong>
          <p>
            The manager records each change first; its controller then brings
            the instance to the new state. Instance phases show the progress.
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
  href: "/assignments" | "/terminal-backends" | "/activity";
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
