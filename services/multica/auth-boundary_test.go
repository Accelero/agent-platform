// Focused native tests run outside upstream packages whose TestMain exits zero
// without PostgreSQL. Only the SQL boundary is a fixture; Auth and handlers are real.
package platformauth

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"reflect"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/go-chi/chi/v5"
	"github.com/golang-jwt/jwt/v5"
	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgconn"
	"github.com/jackc/pgx/v5/pgtype"
	"github.com/multica-ai/multica/server/internal/auth"
	"github.com/multica-ai/multica/server/internal/handler"
	"github.com/multica-ai/multica/server/internal/middleware"
	db "github.com/multica-ai/multica/server/pkg/db/generated"
)

const ownerID = "00000000-0000-0000-0000-000000000001"
const rawMAT = "mat_native_boundary_fixture"
const rawPAT = "mul_native_boundary_fixture"

func uuidValue(value string) pgtype.UUID {
	var result pgtype.UUID
	if err := result.Scan(value); err != nil { panic(err) }
	return result
}

func fixturePAT() db.PersonalAccessToken {
	return db.PersonalAccessToken{ID: uuidValue("00000000-0000-0000-0000-000000000005"),
		UserID: uuidValue(ownerID), Name: "fixture", TokenHash: auth.HashToken(rawPAT),
		TokenPrefix: "mul_fixture", ExpiresAt: pgtype.Timestamptz{Time: time.Now().Add(30*24*time.Hour), Valid: true}}
}

func patValues(pat db.PersonalAccessToken) []any {
	return []any{pat.ID,pat.UserID,pat.Name,pat.TokenHash,pat.TokenPrefix,pat.ExpiresAt,pat.LastUsedAt,pat.Revoked,pat.CreatedAt}
}

type row struct { values []any; err error }
func (r row) Scan(dest ...any) error {
	if r.err != nil { return r.err }
	if len(dest)!=len(r.values) { return fmt.Errorf("fixture Scan arity mismatch") }
	for i, value := range r.values { reflect.ValueOf(dest[i]).Elem().Set(reflect.ValueOf(value)) }
	return nil
}

type emptyRows struct { pgx.Rows }
func (emptyRows) Next() bool { return false }
func (emptyRows) Close() {}
func (emptyRows) Err() error { return nil }

type credentialDB struct { credentialQueries atomic.Int32 }
func (d *credentialDB) Exec(_ context.Context, sql string, _ ...any) (pgconn.CommandTag,error) {
	if !strings.Contains(sql,"UpdatePersonalAccessTokenLastUsed") { return pgconn.CommandTag{},fmt.Errorf("unexpected fixture Exec") }
	return pgconn.NewCommandTag("UPDATE 1"),nil
}
func (d *credentialDB) Query(_ context.Context, sql string, _ ...any) (pgx.Rows,error) {
	d.credentialQueries.Add(1)
	if !strings.Contains(sql,"ListPersonalAccessTokensByUser") { return nil,fmt.Errorf("unexpected fixture Query") }
	return emptyRows{},nil
}
func (d *credentialDB) QueryRow(_ context.Context, sql string, args ...any) pgx.Row {
	if strings.Contains(sql,"GetTaskTokenByHash") {
		if args[0]!=auth.HashToken(rawMAT) { return row{err:pgx.ErrNoRows} }
		return row{values:[]any{uuidValue("00000000-0000-0000-0000-000000000006"),auth.HashToken(rawMAT),
			uuidValue("00000000-0000-0000-0000-000000000002"),uuidValue("00000000-0000-0000-0000-000000000003"),
			uuidValue("00000000-0000-0000-0000-000000000004"),uuidValue(ownerID),
			pgtype.Timestamptz{Time:time.Now().Add(time.Hour),Valid:true},pgtype.Timestamptz{}}}
	}
	d.credentialQueries.Add(1)
	switch {
	case strings.Contains(sql,"GetPersonalAccessTokenByHash"):
		if args[0]!=auth.HashToken(rawPAT) { return row{err:pgx.ErrNoRows} }
		return row{values:patValues(fixturePAT())}
	case strings.Contains(sql,"CreatePersonalAccessToken"):
		pat:=fixturePAT()
		pat.UserID=args[0].(pgtype.UUID);pat.Name=args[1].(string);pat.TokenHash=args[2].(string)
		pat.TokenPrefix=args[3].(string);pat.ExpiresAt=args[4].(pgtype.Timestamptz)
		return row{values:patValues(pat)}
	case strings.Contains(sql,"RevokePersonalAccessToken"):
		return row{values:[]any{auth.HashToken(rawPAT)}}
	case strings.Contains(sql,"-- name: GetUser :one"):
		user:=db.User{ID:uuidValue(ownerID),Name:"Fixture",Email:"fixture@example.invalid"}
		return row{values:[]any{user.ID,user.Name,user.Email,user.AvatarUrl,user.CreatedAt,user.UpdatedAt,
			user.OnboardedAt,user.OnboardingQuestionnaire,user.CloudWaitlistEmail,user.CloudWaitlistReason,
			user.StarterContentState,user.Language,user.ProfileDescription,user.Timezone}}
	default:
		return row{err:fmt.Errorf("unexpected fixture QueryRow")}
	}
}

type endpoint struct {
	name, method, path, body string
	call func(*handler.Handler) http.HandlerFunc
	patStatus, jwtStatus int
}

var endpoints=[]endpoint{
	{"list","GET","/api/tokens","",func(h *handler.Handler)http.HandlerFunc{return h.ListPersonalAccessTokens},200,200},
	{"create","POST","/api/tokens",`{"name":"native-boundary-fixture","expires_in_days":1}`,func(h *handler.Handler)http.HandlerFunc{return h.CreatePersonalAccessToken},201,201},
	{"renew","POST","/api/tokens/current/renew","",func(h *handler.Handler)http.HandlerFunc{return h.RenewCurrentPersonalAccessToken},200,400},
	{"revoke","DELETE","/api/tokens/00000000-0000-0000-0000-000000000005","",func(h *handler.Handler)http.HandlerFunc{return h.RevokePersonalAccessToken},204,204},
	{"cli-token","POST","/api/cli-token","",func(h *handler.Handler)http.HandlerFunc{return h.IssueCliToken},200,200},
}

func invoke(ep endpoint, raw, forgedSource string, routerGuard bool, database *credentialDB) *httptest.ResponseRecorder {
	queries:=db.New(database)
	h:=&handler.Handler{Queries:queries}
	var next http.Handler=ep.call(h)
	if routerGuard { next=handler.RequireHumanActor(next) }
	chain:=middleware.Auth(queries,nil,nil,nil)(next)
	req:=httptest.NewRequest(ep.method,ep.path,strings.NewReader(ep.body))
	if ep.name=="revoke" {
		route:=chi.NewRouteContext()
		route.URLParams.Add("id","00000000-0000-0000-0000-000000000005")
		req=req.WithContext(context.WithValue(req.Context(),chi.RouteCtxKey,route))
	}
	req.Header.Set("Authorization","Bearer "+raw)
	req.Header.Set("X-Actor-Source",forgedSource)
	req.Header.Set("X-Agent-ID","forged-agent")
	req.Header.Set("X-Task-ID","forged-task")
	req.Header.Set("X-User-ID","forged-owner")
	response:=httptest.NewRecorder()
	chain.ServeHTTP(response,req)
	return response
}

func TestHermesMATCannotAdministerOwnerCredentials(t *testing.T) {
	for _, ep:=range endpoints {
		for _, guard:=range []bool{false,true} {
			for _, forged:=range []string{"","member","future_kind"} {
				t.Run(fmt.Sprintf("%s/guard=%t/source=%s",ep.name,guard,forged),func(t *testing.T){
					database:=&credentialDB{}
					response:=invoke(ep,rawMAT,forged,guard,database)
					if response.Code!=http.StatusForbidden { t.Fatalf("status=%d, want403",response.Code) }
					if database.credentialQueries.Load()!=0 { t.Fatal("MAT reached owner credential storage") }
				})
			}
		}
	}
}

func TestHermesHumanPATAndJWTStillWork(t *testing.T) {
	t.Setenv("JWT_SECRET",strings.Repeat("fixture-secret-",4))
	jwtToken,err:=jwt.NewWithClaims(jwt.SigningMethodHS256,jwt.MapClaims{
		"sub":ownerID,"exp":time.Now().Add(time.Hour).Unix(),
	}).SignedString(auth.JWTSecret())
	if err!=nil { t.Fatal("could not sign fixture session") }
	for _, ep:=range endpoints {
		for _, credential:=range []struct{name,raw string;status int}{{"PAT",rawPAT,ep.patStatus},{"JWT",jwtToken,ep.jwtStatus}} {
			t.Run(ep.name+"/"+credential.name,func(t *testing.T){
				// Auth must discard the client's machine stamp for a real PAT/JWT.
				response:=invoke(ep,credential.raw,"task_token",true,&credentialDB{})
				if response.Code!=credential.status { t.Fatalf("status=%d, want%d",response.Code,credential.status) }
				if ep.name=="create" || ep.name=="cli-token" {
					var payload struct{Token string `json:"token"`}
					if json.Unmarshal(response.Body.Bytes(),&payload)!=nil || payload.Token=="" { t.Fatal("human credential issuance failed") }
				}
			})
		}
	}
}

func TestHermesCloudMachineHandlerBackstop(t *testing.T) {
	for _, ep:=range endpoints {
		t.Run(ep.name,func(t *testing.T){
			req:=httptest.NewRequest(ep.method,ep.path,strings.NewReader(ep.body))
			req.Header.Set("X-User-ID",ownerID)
			req.Header.Set("X-Actor-Source","cloud_pat")
			response:=httptest.NewRecorder()
			ep.call(&handler.Handler{}).ServeHTTP(response,req)
			if response.Code!=http.StatusForbidden { t.Fatalf("status=%d,want403",response.Code) }
		})
	}
}

func TestHermesMATOrdinaryTrafficKeepsBoundActor(t *testing.T) {
	queries:=db.New(&credentialDB{})
	next:=http.HandlerFunc(func(w http.ResponseWriter,r *http.Request){
		if r.Header.Get("X-Actor-Source")!="task_token" || r.Header.Get("X-User-ID")!=ownerID ||
			r.Header.Get("X-Agent-ID")!="00000000-0000-0000-0000-000000000003" ||
			r.Header.Get("X-Task-ID")!="00000000-0000-0000-0000-000000000002" {
			t.Fatal("native task actor binding changed")
		}
		w.WriteHeader(http.StatusOK)
	})
	req:=httptest.NewRequest("GET","/api/issues/own",nil)
	req.Header.Set("Authorization","Bearer "+rawMAT)
	req.Header.Set("X-Actor-Source","member")
	response:=httptest.NewRecorder()
	middleware.Auth(queries,nil,nil,nil)(next).ServeHTTP(response,req)
	if response.Code!=200 { t.Fatalf("ordinary task auth status=%d,want200",response.Code) }
}
