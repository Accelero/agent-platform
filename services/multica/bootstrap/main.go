// Offline administrator command. No HTTP route exposes these capabilities.
package main

import (
	"context"
	_ "embed"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"regexp"
	"strings"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgtype"
	"github.com/multica-ai/multica/server/internal/auth"
	"github.com/multica-ai/multica/server/internal/issuestatus"
	db "github.com/multica-ai/multica/server/pkg/db/generated"
)

//go:embed migrations.txt
var requiredMigrations string

type request struct {
	DaemonID        string `json:"daemon_id"`
	Action          string `json:"action"`
	Installation    string `json:"installation"`
	Email           string `json:"email"`
	Name            string `json:"name"`
	Slug            string `json:"slug"`
	Prefix          string `json:"prefix"`
	Daemon          string `json:"daemon"`
	Profile         string `json:"profile"`
	Previous        string `json:"previous"`
	PreviousProfile string `json:"previous_profile"`
	User            string `json:"user"`
	Workspace       string `json:"workspace"`
	Force           bool   `json:"force"`
}
type result struct {
	State     string `json:"state,omitempty"`
	User      string `json:"user,omitempty"`
	Workspace string `json:"workspace,omitempty"`
	Daemon    string `json:"daemon,omitempty"`
	Profile   string `json:"profile,omitempty"`
}

func uuid(s string) pgtype.UUID { var id pgtype.UUID; _ = id.Scan(s); return id }
func idString(id pgtype.UUID) string {
	return fmt.Sprintf("%x-%x-%x-%x-%x", id.Bytes[0:4], id.Bytes[4:6], id.Bytes[6:8], id.Bytes[8:10], id.Bytes[10:16])
}
func status(ctx context.Context, conn *pgx.Conn, key string) (result, error) {
	var missing int
	if err := conn.QueryRow(ctx, `SELECT count(*) FROM unnest($1::text[]) expected(version) LEFT JOIN schema_migrations m USING(version) WHERE m.version IS NULL`, strings.Fields(requiredMigrations)).Scan(&missing); err != nil {
		return result{}, err
	}
	if missing != 0 {
		return result{}, errors.New("native migrations pending")
	}
	var exists bool
	if err := conn.QueryRow(ctx, `SELECT to_regclass('public.agent_platform_bootstrap') IS NOT NULL`).Scan(&exists); err != nil {
		return result{}, err
	}
	if exists {
		var r result
		var complete bool
		err := conn.QueryRow(ctx, `SELECT user_id::text, workspace_id::text, complete FROM agent_platform_bootstrap WHERE installation=$1`, key).Scan(&r.User, &r.Workspace, &complete)
		if err == nil {
			r.State = "pending"
			if complete {
				r.State = "initialized"
			}
			return r, nil
		}
		if !errors.Is(err, pgx.ErrNoRows) {
			return r, err
		}
	}
	var empty bool
	err := conn.QueryRow(ctx, `SELECT NOT EXISTS(SELECT 1 FROM "user") AND NOT EXISTS(SELECT 1 FROM workspace)`).Scan(&empty)
	state := "initialized"
	if empty {
		state = "empty"
	}
	return result{State: state}, err
}
func ensure(ctx context.Context, conn *pgx.Conn, in request) (result, error) {
	if in.Installation == "" || in.Email == "" || !regexp.MustCompile(`^mul_[0-9a-f]{40}$`).MatchString(in.Daemon) || !regexp.MustCompile(`^mul_[0-9a-f]{40}$`).MatchString(in.Profile) || in.Daemon == in.Profile {
		return result{}, errors.New("invalid input")
	}
	tx, err := conn.Begin(ctx)
	if err != nil {
		return result{}, err
	}
	defer tx.Rollback(ctx)
	if _, err = tx.Exec(ctx, `LOCK TABLE "user", workspace IN SHARE ROW EXCLUSIVE MODE`); err != nil {
		return result{}, err
	}
	if _, err = tx.Exec(ctx, `CREATE TABLE IF NOT EXISTS agent_platform_bootstrap (installation text PRIMARY KEY, user_id uuid NOT NULL, workspace_id uuid NOT NULL, complete boolean NOT NULL DEFAULT false)`); err != nil {
		return result{}, err
	}
	q := db.New(tx)
	var userID, workspaceID pgtype.UUID
	err = tx.QueryRow(ctx, `SELECT user_id,workspace_id FROM agent_platform_bootstrap WHERE installation=$1`, in.Installation).Scan(&userID, &workspaceID)
	if errors.Is(err, pgx.ErrNoRows) {
		var empty bool
		if err = tx.QueryRow(ctx, `SELECT NOT EXISTS(SELECT 1 FROM "user") AND NOT EXISTS(SELECT 1 FROM workspace)`).Scan(&empty); err != nil {
			return result{}, err
		}
		if !empty {
			// Explicit repair may adopt only the saved native owner's enrollment. A
			// configured email alone is never authority to seize a collaborator runtime.
			if !in.Force || in.Previous == "" || !uuid(in.Workspace).Valid {
				return result{}, errors.New("foreign state")
			}
			err = tx.QueryRow(ctx, `SELECT u.id,m.workspace_id FROM "user" u JOIN personal_access_token p ON p.user_id=u.id JOIN member m ON m.user_id=u.id WHERE p.token_hash=$1 AND u.email=$2 AND m.workspace_id=$3 AND m.role='owner'`, auth.HashToken(in.Previous), strings.ToLower(in.Email), uuid(in.Workspace)).Scan(&userID, &workspaceID)
			if err != nil {
				return result{}, errors.New("owner verification failed")
			}
		} else {
			if !regexp.MustCompile(`^[a-z0-9]+(-[a-z0-9]+)*$`).MatchString(in.Slug) || !regexp.MustCompile(`^[A-Z][A-Z0-9]{0,9}$`).MatchString(in.Prefix) || in.Name == "" {
				return result{}, errors.New("invalid workspace")
			}
			u, e := q.CreateUser(ctx, db.CreateUserParams{Name: "Platform administrator", Email: strings.ToLower(in.Email)})
			if e != nil {
				return result{}, e
			}
			userID = u.ID
			w, e := q.CreateWorkspace(ctx, db.CreateWorkspaceParams{Name: in.Name, Slug: in.Slug, IssuePrefix: in.Prefix})
			if e != nil {
				return result{}, e
			}
			workspaceID = w.ID
			if _, e = q.CreateMember(ctx, db.CreateMemberParams{WorkspaceID: w.ID, UserID: u.ID, Role: "owner"}); e != nil {
				return result{}, e
			}
			if e = issuestatus.Ensure(ctx, q, w.ID); e != nil {
				return result{}, e
			}
			if _, e = q.MarkUserOnboarded(ctx, u.ID); e != nil {
				return result{}, e
			}
		}
		if _, err = tx.Exec(ctx, `INSERT INTO agent_platform_bootstrap(installation,user_id,workspace_id) VALUES($1,$2,$3)`, in.Installation, userID, workspaceID); err != nil {
			return result{}, err
		}
	} else if err != nil {
		return result{}, err
	}
	// A journal never permits ownership transfer or resurrects deleted membership.
	var owned bool
	if err = tx.QueryRow(ctx, `SELECT EXISTS(SELECT 1 FROM member WHERE user_id=$1 AND workspace_id=$2 AND role='owner')`, userID, workspaceID).Scan(&owned); err != nil || !owned {
		return result{}, errors.New("workspace owner changed")
	}
	var runtimes, matching int
	if err = tx.QueryRow(ctx, `SELECT count(*), count(*) FILTER(WHERE daemon_id=$3) FROM agent_runtime WHERE owner_id=$1 AND workspace_id=$2 AND provider='hermes'`, userID, workspaceID, in.DaemonID).Scan(&runtimes, &matching); err != nil {
		return result{}, err
	}
	if runtimes > 0 && matching == 0 {
		return result{}, errors.New("native daemon identity changed")
	}
	// A recovered journal binds to the original IDs, irrespective of later seed edits.
	for i, raw := range []string{in.Daemon, in.Profile} {
		var owner pgtype.UUID
		var revoked bool
		e := tx.QueryRow(ctx, `SELECT user_id,revoked FROM personal_access_token WHERE token_hash=$1`, auth.HashToken(raw)).Scan(&owner, &revoked)
		if e == nil {
			if owner != userID || revoked {
				return result{}, errors.New("credential conflict")
			}
			continue
		}
		if !errors.Is(e, pgx.ErrNoRows) {
			return result{}, e
		}
		name := []string{"Platform Hermes daemon", "Platform Hermes CLI"}[i]
		if _, e = q.CreatePersonalAccessToken(ctx, db.CreatePersonalAccessTokenParams{UserID: userID, Name: name, TokenHash: auth.HashToken(raw), TokenPrefix: raw[:12], ExpiresAt: pgtype.Timestamptz{Time: time.Now().Add(90 * 24 * time.Hour), Valid: true}}); e != nil {
			return result{}, e
		}
	}
	if err = tx.Commit(ctx); err != nil {
		return result{}, err
	}
	return result{State: "pending", User: idString(userID), Workspace: idString(workspaceID)}, nil
}
func run(in request) (result, error) {
	if in.Action == "generate" {
		a, e := auth.GeneratePATToken()
		if e != nil {
			return result{}, e
		}
		b, e := auth.GeneratePATToken()
		return result{Daemon: a, Profile: b}, e
	}
	ctx, cancel := context.WithTimeout(context.Background(), 25*time.Second)
	defer cancel()
	conn, err := pgx.Connect(ctx, os.Getenv("DATABASE_URL"))
	if err != nil {
		return result{}, err
	}
	defer conn.Close(ctx)
	switch in.Action {
	case "inspect":
		return status(ctx, conn, in.Installation)
	case "ensure":
		return ensure(ctx, conn, in)
	case "complete":
		tag, e := conn.Exec(ctx, `UPDATE agent_platform_bootstrap SET complete=true WHERE installation=$1 AND user_id=$2 AND workspace_id=$3`, in.Installation, uuid(in.User), uuid(in.Workspace))
		if e != nil {
			return result{}, e
		}
		if tag.RowsAffected() != 1 {
			return result{}, errors.New("journal missing")
		}
		// Retire only the explicitly replaced owner PATs, after reconnect succeeds.
		for _, raw := range []string{in.Previous, in.PreviousProfile} {
			if raw != "" && raw != in.Daemon && raw != in.Profile {
				if _, e = conn.Exec(ctx, `UPDATE personal_access_token SET revoked=true WHERE token_hash=$1 AND user_id=$2`, auth.HashToken(raw), uuid(in.User)); e != nil {
					return result{}, e
				}
			}
		}
		return result{State: "initialized"}, nil
	default:
		return result{}, errors.New("unknown operation")
	}
}
func main() {
	var in request
	if json.NewDecoder(io.LimitReader(os.Stdin, 65536)).Decode(&in) != nil {
		fmt.Fprintln(os.Stderr, "Invalid bootstrap request.")
		os.Exit(1)
	}
	out, err := run(in)
	if err != nil {
		fmt.Fprintln(os.Stderr, "Native bootstrap failed; private details suppressed.")
		os.Exit(1)
	}
	_ = json.NewEncoder(os.Stdout).Encode(out)
}
