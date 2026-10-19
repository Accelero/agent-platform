package hermesrouteresolver

import (
	"bufio"
	"context"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/tls"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/json"
	"encoding/pem"
	"io"
	"math/big"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync/atomic"
	"testing"
	"time"
)

const testIssuer = "https://sso.example.org/realms/agents"
const ownerA = "00000000-0000-4000-8000-000000000001"
const ownerB = "00000000-0000-4000-8000-000000000002"

type fixture struct {
	config         *Config
	managerCalls   atomic.Int64
	sessionCalls   atomic.Int64
	revoke         atomic.Bool
	sessionRevoked atomic.Bool
	switchIdentity atomic.Bool
	outage         atomic.Bool
	fault          func(http.ResponseWriter)
}

func ready(subject string) decision {
	instance := ownerA
	if subject == "owner-b" {
		instance = ownerB
	}
	return decision{Status: "ready", InstanceID: instance, RouteGeneration: 1, AuthorizationVersion: 1,
		Destination: &destination{Namespace: "hi-" + strings.Repeat("a", 24) + "-" + strings.ReplaceAll(instance, "-", ""), Service: "hermes", Port: 8080, Scheme: "http"}}
}

func newFixture(t *testing.T) *fixture {
	t.Helper()
	certPath, keyPath, certificate, roots := certificates(t)
	f := &fixture{config: CreateConfig()}
	server := httptest.NewUnstartedServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if len(r.TLS.VerifiedChains) == 0 {
			t.Error("missing client mTLS")
		}
		w.Header().Set("Cache-Control", "no-store")
		w.Header().Set("Content-Type", "application/json")
		if r.Method != http.MethodPost {
			t.Error("unexpected internal method")
		}
		if r.URL.Path == "/internal/v1/sessions/authorize" {
			f.sessionCalls.Add(1)
			if f.sessionRevoked.Load() {
				w.WriteHeader(401)
				io.WriteString(w, `{"error":"session_expired"}`)
				return
			}
			var body struct {
				SessionID string `json:"sessionId"`
			}
			if json.NewDecoder(r.Body).Decode(&body) != nil || body.SessionID != strings.Repeat("c", 64) {
				t.Error("invalid session lookup")
			}
			subject := "owner-a"
			if f.switchIdentity.Load() {
				subject = "owner-b"
			}
			json.NewEncoder(w).Encode(refreshedEvidence{Identity: identity{Issuer: testIssuer, Subject: subject}, AccessToken: "fixture-token-" + subject})
			return
		}
		if r.URL.Path != "/internal/v1/routes/resolve" {
			t.Error("unexpected internal path")
		}
		f.managerCalls.Add(1)
		if f.fault != nil {
			f.fault(w)
			return
		}
		if f.outage.Load() {
			w.WriteHeader(503)
			io.WriteString(w, `{"status":"unavailable","reason":"dependency_unavailable"}`)
			return
		}
		var body struct {
			Identity identity `json:"identity"`
		}
		if json.NewDecoder(r.Body).Decode(&body) != nil {
			t.Error("invalid resolver body")
		}
		if r.Header.Get("Authorization") != "Bearer fixture-token-"+body.Identity.Subject {
			w.WriteHeader(401)
			io.WriteString(w, `{"error":"invalid_identity_evidence"}`)
			return
		}
		if (f.revoke.Load() && body.Identity.Subject == "owner-a") || (body.Identity.Subject != "owner-a" && body.Identity.Subject != "owner-b") {
			w.WriteHeader(403)
			io.WriteString(w, `{"status":"denied","reason":"access_denied"}`)
			return
		}
		json.NewEncoder(w).Encode(ready(body.Identity.Subject))
	}))
	server.TLS = &tls.Config{Certificates: []tls.Certificate{certificate}, ClientAuth: tls.RequireAndVerifyClientCert, ClientCAs: roots, MinVersion: tls.VersionTLS12}
	server.StartTLS()
	t.Cleanup(server.Close)
	f.config.ManagerURL = server.URL + "/internal/v1/routes/resolve"
	f.config.SessionURL = server.URL + "/internal/v1/sessions/authorize"
	f.config.Issuer = testIssuer
	f.config.InstallationPrefix = strings.Repeat("a", 24)
	f.config.CAFile, f.config.CertFile, f.config.KeyFile = certPath, certPath, keyPath
	f.config.RecheckIntervalMs = 100
	return f
}

func certificates(t *testing.T) (string, string, tls.Certificate, *x509.CertPool) {
	t.Helper()
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	template := &x509.Certificate{SerialNumber: big.NewInt(1), Subject: pkix.Name{CommonName: "hermes-router-fixture"},
		NotBefore: time.Now().Add(-time.Hour), NotAfter: time.Now().Add(time.Hour),
		IsCA: true, BasicConstraintsValid: true, KeyUsage: x509.KeyUsageCertSign | x509.KeyUsageDigitalSignature,
		ExtKeyUsage: []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth, x509.ExtKeyUsageClientAuth},
		IPAddresses: []net.IP{net.ParseIP("127.0.0.1")}, DNSNames: []string{"localhost"}}
	der, err := x509.CreateCertificate(rand.Reader, template, template, &key.PublicKey, key)
	if err != nil {
		t.Fatal(err)
	}
	encodedKey, err := x509.MarshalPKCS8PrivateKey(key)
	if err != nil {
		t.Fatal(err)
	}
	certPEM := pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: der})
	keyPEM := pem.EncodeToMemory(&pem.Block{Type: "PRIVATE KEY", Bytes: encodedKey})
	directory := t.TempDir()
	certPath, keyPath := filepath.Join(directory, "tls.crt"), filepath.Join(directory, "tls.key")
	if err := os.WriteFile(certPath, certPEM, 0600); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(keyPath, keyPEM, 0600); err != nil {
		t.Fatal(err)
	}
	certificate, err := tls.X509KeyPair(certPEM, keyPEM)
	if err != nil {
		t.Fatal(err)
	}
	roots := x509.NewCertPool()
	roots.AppendCertsFromPEM(certPEM)
	return certPath, keyPath, certificate, roots
}

func request(subject string) *http.Request {
	req := httptest.NewRequest(http.MethodGet, "https://hermes.example.org/api/settings", nil)
	req.Header.Set(issuerHeader, testIssuer)
	req.Header.Set(subjectHeader, subject)
	req.Header.Set(evidenceHeader, "fixture-token-"+subject)
	req.Header.Set(sessionHeader, strings.Repeat("c", 64))
	return req
}

func build(t *testing.T, f *fixture, next http.Handler) http.Handler {
	t.Helper()
	handler, err := New(context.Background(), next, f.config, "fixture")
	if err != nil {
		t.Fatal(err)
	}
	return handler
}

func TestSameHostAndPathSelectDistinctRegisteredOwnersWithoutCaching(t *testing.T) {
	f := newFixture(t)
	handler := build(t, f, http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		for _, name := range []string{issuerHeader, subjectHeader, evidenceHeader, sessionHeader} {
			if r.Header.Get(name) != "" {
				t.Error("identity evidence reached native backend")
			}
		}
		io.WriteString(w, r.Header.Get(instanceHeader)+"/"+r.Header.Get(generationHeader))
	}))
	for _, subject := range []string{"owner-a", "owner-b", "owner-a"} {
		req := request(subject)
		req.Header.Set(instanceHeader, "forged-owner")
		req.Header.Set(generationHeader, "999")
		res := httptest.NewRecorder()
		handler.ServeHTTP(res, req)
		if res.Code != 200 || res.Body.String() != ready(subject).InstanceID+"/1" {
			t.Fatalf("wrong owner decision: %d %s", res.Code, res.Body.String())
		}
	}
	if f.managerCalls.Load() != 3 {
		t.Fatal("positive assignment cache detected")
	}
	f.revoke.Store(true)
	res := httptest.NewRecorder()
	handler.ServeHTTP(res, request("owner-a"))
	if res.Code != 403 {
		t.Fatalf("revoked owner reached backend: %d", res.Code)
	}
	res = httptest.NewRecorder()
	handler.ServeHTTP(res, request("owner-b"))
	if res.Code != 200 {
		t.Fatal("revoking one owner affected another owner")
	}
}

func TestLookupDeadlineIsBoundedAndDoesNotReachBackend(t *testing.T) {
	f := newFixture(t)
	f.fault = func(w http.ResponseWriter) { time.Sleep(3 * time.Second); json.NewEncoder(w).Encode(ready("owner-a")) }
	handler := build(t, f, http.HandlerFunc(func(http.ResponseWriter, *http.Request) { t.Fatal("timed-out resolution reached backend") }))
	started := time.Now()
	res := httptest.NewRecorder()
	handler.ServeHTTP(res, request("owner-a"))
	if res.Code != 503 || time.Since(started) > 2500*time.Millisecond {
		t.Fatalf("lookup deadline failed: %d, %s", res.Code, time.Since(started))
	}
}

func TestMissingForgedAndDuplicateIdentityEvidenceFailsClosed(t *testing.T) {
	f := newFixture(t)
	handler := build(t, f, http.HandlerFunc(func(http.ResponseWriter, *http.Request) { t.Fatal("unauthorized backend request") }))
	for _, mutate := range []func(*http.Request){
		func(r *http.Request) { r.Header.Del(evidenceHeader) },
		func(r *http.Request) { r.Header.Set(evidenceHeader, "forged-token") },
		func(r *http.Request) { r.Header.Set(issuerHeader, "https://foreign.example.org") },
		func(r *http.Request) { r.Header.Add(subjectHeader, "owner-b") },
		func(r *http.Request) { r.Header.Set(subjectHeader, "") },
		func(r *http.Request) { r.Header.Set(evidenceHeader, strings.Repeat("x", maxBody+1)) },
	} {
		req := request("owner-a")
		mutate(req)
		res := httptest.NewRecorder()
		handler.ServeHTTP(res, req)
		if res.Code != 401 {
			t.Fatalf("invalid identity status %d", res.Code)
		}
	}
	res := httptest.NewRecorder()
	handler.ServeHTTP(res, request("unassigned"))
	if res.Code != 403 {
		t.Fatal("unassigned owner was not denied")
	}
}

func TestMalformedManagerDecisionsNeverReachBackend(t *testing.T) {
	cases := map[string]func(http.ResponseWriter){
		"arbitrary-url": func(w http.ResponseWriter) {
			io.WriteString(w, `{"status":"ready","url":"http://attacker.example.org"}`)
		},
		"missing-destination": func(w http.ResponseWriter) {
			result := ready("owner-a")
			result.Destination = nil
			json.NewEncoder(w).Encode(result)
		},
		"foreign-namespace": func(w http.ResponseWriter) {
			result := ready("owner-a")
			result.Destination.Namespace = "system"
			json.NewEncoder(w).Encode(result)
		},
		"wrong-port": func(w http.ResponseWriter) {
			result := ready("owner-a")
			result.Destination.Port = 443
			json.NewEncoder(w).Encode(result)
		},
		"invalid-generation": func(w http.ResponseWriter) {
			result := ready("owner-a")
			result.RouteGeneration = 0
			json.NewEncoder(w).Encode(result)
		},
		"wrong-status": func(w http.ResponseWriter) {
			result := ready("owner-a")
			result.Status = "pending"
			json.NewEncoder(w).Encode(result)
		},
		"trailing-json": func(w http.ResponseWriter) { json.NewEncoder(w).Encode(ready("owner-a")); io.WriteString(w, `{}`) },
		"oversized":     func(w http.ResponseWriter) { io.WriteString(w, strings.Repeat("x", maxBody+1)) },
		"cacheable": func(w http.ResponseWriter) {
			w.Header().Del("Cache-Control")
			json.NewEncoder(w).Encode(ready("owner-a"))
		},
		"denial-with-destination": func(w http.ResponseWriter) { w.WriteHeader(403); json.NewEncoder(w).Encode(ready("owner-a")) },
		"redirect": func(w http.ResponseWriter) {
			w.Header().Set("Location", "https://attacker.example.org")
			w.WriteHeader(302)
			io.WriteString(w, `{}`)
		},
	}
	for name, fault := range cases {
		t.Run(name, func(t *testing.T) {
			f := newFixture(t)
			f.fault = fault
			handler := build(t, f, http.HandlerFunc(func(http.ResponseWriter, *http.Request) { t.Fatal("malformed decision reached backend") }))
			res := httptest.NewRecorder()
			handler.ServeHTTP(res, request("owner-a"))
			if res.Code != 503 {
				t.Fatalf("expected fail closed, got %d", res.Code)
			}
		})
	}
}

func TestResolverDoesNotBufferOrReplayNativeRequests(t *testing.T) {
	f := newFixture(t)
	var calls int
	handler := build(t, f, http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		calls++
		if r.Header.Get("Authorization") != "Bearer native-session" {
			t.Error("native authentication removed")
		}
		if r.Header.Get("Content-Type") != "application/octet-stream" {
			t.Error("native content type changed")
		}
		data, err := io.ReadAll(r.Body)
		if err != nil || string(data) != "native-upload" {
			t.Error("native body changed")
		}
		w.WriteHeader(502)
	}))
	for _, method := range []string{"POST", "PUT", "PATCH", "DELETE"} {
		req := request("owner-a")
		req.Method = method
		req.Body = io.NopCloser(strings.NewReader("native-upload"))
		req.Header.Set("Content-Type", "application/octet-stream")
		req.Header.Set("Authorization", "Bearer native-session")
		res := httptest.NewRecorder()
		handler.ServeHTTP(res, req)
		if res.Code != 502 {
			t.Error("native error was not preserved")
		}
	}
	if calls != 4 {
		t.Fatal("native mutation was replayed")
	}
}

func TestConfigurationRejectsUnsafeEndpointsAndMissingTLS(t *testing.T) {
	f := newFixture(t)
	for _, endpoint := range []string{"http://manager.example.org/internal/v1/routes/resolve", "https://user@manager.example.org/internal/v1/routes/resolve", "https://manager.example.org/internal/v1/routes/resolve?target=other", "https://manager.example.org/other"} {
		copied := *f.config
		copied.ManagerURL = endpoint
		if _, err := New(context.Background(), http.NotFoundHandler(), &copied, "test"); err == nil {
			t.Fatal("unsafe endpoint accepted")
		}
	}
	copied := *f.config
	copied.RecheckIntervalMs = 5001
	if _, err := New(context.Background(), http.NotFoundHandler(), &copied, "test"); err == nil {
		t.Fatal("unbounded revocation accepted")
	}
	copied = *f.config
	copied.CertFile = "missing"
	if _, err := New(context.Background(), http.NotFoundHandler(), &copied, "test"); err == nil {
		t.Fatal("missing client certificate accepted")
	}
}

func TestExistingWebSocketClosesOnAssignmentSessionIdentityAndDependencyChange(t *testing.T) {
	for _, kind := range []string{"assignment", "session", "identity-switch", "manager-outage"} {
		t.Run(kind, func(t *testing.T) {
			f := newFixture(t)
			backend := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				connection, buffer, err := w.(http.Hijacker).Hijack()
				if err != nil {
					t.Error(err)
					return
				}
				defer connection.Close()
				io.WriteString(buffer, "HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\n")
				buffer.Flush()
				io.Copy(io.Discard, connection)
			})
			handler := build(t, f, backend)
			// The fixture models the parent ForwardAuth result. Actual Yaegi and
			// child routing are independently exercised by the compatibility test.
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				for name, values := range request("owner-a").Header {
					r.Header[name] = values
				}
				handler.ServeHTTP(w, r)
			}))
			defer server.Close()
			connection, err := net.Dial("tcp", strings.TrimPrefix(server.URL, "http://"))
			if err != nil {
				t.Fatal(err)
			}
			defer connection.Close()
			io.WriteString(connection, "GET /api/ws HTTP/1.1\r\nHost: hermes.example.org\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\n")
			reader := bufio.NewReader(connection)
			response, err := http.ReadResponse(reader, nil)
			if err != nil || response.StatusCode != 101 {
				t.Fatalf("upgrade failed: %v", err)
			}
			// A valid idle connection survives repeated reauthorization.
			deadline := time.Now().Add(time.Second)
			for f.sessionCalls.Load() < 2 && time.Now().Before(deadline) {
				time.Sleep(10 * time.Millisecond)
			}
			if f.sessionCalls.Load() < 2 {
				t.Fatal("no established-connection checks")
			}
			switch kind {
			case "assignment":
				f.revoke.Store(true)
			case "session":
				f.sessionRevoked.Store(true)
			case "identity-switch":
				f.switchIdentity.Store(true)
			case "manager-outage":
				f.outage.Store(true)
			}
			started := time.Now()
			connection.SetReadDeadline(time.Now().Add(2 * time.Second))
			_, err = reader.ReadByte()
			if err == nil {
				t.Fatal("revoked connection remained usable")
			}
			if timeout, ok := err.(net.Error); ok && timeout.Timeout() {
				t.Fatal("revoked connection did not close")
			}
			if time.Since(started) > time.Second {
				t.Fatal("revocation exceeded fixture bound")
			}
		})
	}
}
