// Package hermesrouteresolver implements the Traefik/Yaegi route-selection boundary.
// It has no SQL, Kubernetes, OIDC or user-selected upstream implementation.
package hermesrouteresolver

import (
	"bufio"
	"bytes"
	"context"
	"crypto/tls"
	"crypto/x509"
	"encoding/json"
	"errors"
	"io"
	"net"
	"net/http"
	"net/url"
	"os"
	"regexp"
	"strconv"
	"strings"
	"sync"
	"time"
)

const (
	issuerHeader     = "X-Hermes-Verified-Issuer"
	subjectHeader    = "X-Hermes-Verified-Subject"
	evidenceHeader   = "X-Hermes-Verified-Evidence"
	sessionHeader    = "X-Hermes-Verified-Session"
	instanceHeader   = "X-Hermes-Instance"
	generationHeader = "X-Hermes-Route-Generation"
	maxBody          = 16 * 1024
)

// Config is installation configuration, never supplied by a browser or agent.
type Config struct {
	ManagerURL         string `json:"managerURL"`
	SessionURL         string `json:"sessionURL"`
	Issuer             string `json:"issuer"`
	InstallationPrefix string `json:"installationPrefix"`
	CAFile             string `json:"caFile"`
	CertFile           string `json:"certFile"`
	KeyFile            string `json:"keyFile"`
	RecheckIntervalMs  int    `json:"recheckIntervalMs"`
	AllowedPorts       []int  `json:"allowedPorts"`
}

func CreateConfig() *Config {
	return &Config{RecheckIntervalMs: 5000, AllowedPorts: []int{8080}}
}

type identity struct {
	Issuer  string `json:"issuer"`
	Subject string `json:"subject"`
}

type destination struct {
	Namespace string `json:"namespace"`
	Service   string `json:"service"`
	Port      int    `json:"port"`
	Scheme    string `json:"scheme"`
}

type decision struct {
	Status               string       `json:"status"`
	Reason               string       `json:"reason,omitempty"`
	Error                string       `json:"error,omitempty"`
	InstanceID           string       `json:"instanceId,omitempty"`
	RouteGeneration      int64        `json:"routeGeneration,omitempty"`
	AuthorizationVersion int64        `json:"authorizationVersion,omitempty"`
	Destination          *destination `json:"destination,omitempty"`
}

type refreshedEvidence struct {
	Identity    identity `json:"identity"`
	AccessToken string   `json:"accessToken"`
}

type middleware struct {
	next   http.Handler
	client *http.Client
	config Config
}

var uuidPattern = regexp.MustCompile(`^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$`)
var prefixPattern = regexp.MustCompile(`^[0-9a-f]{24}$`)
var sessionPattern = regexp.MustCompile(`^[0-9a-f]{64}$`)
var servicePattern = regexp.MustCompile(`^[a-z]([-a-z0-9]*[a-z0-9])?$`)

// New requires mTLS for BOTH manager resolution and periodic session checks.
// No redirect, proxy environment variable or untrusted URL can choose a target.
func New(_ context.Context, next http.Handler, config *Config, _ string) (http.Handler, error) {
	if config == nil || !validEndpoint(config.ManagerURL, "/internal/v1/routes/resolve") ||
		!validEndpoint(config.SessionURL, "/internal/v1/sessions/authorize") ||
		!validIssuer(config.Issuer) || !prefixPattern.MatchString(config.InstallationPrefix) ||
		config.RecheckIntervalMs < 100 || config.RecheckIntervalMs > 5000 || len(config.AllowedPorts) == 0 {
		return nil, errors.New("invalid Hermes resolver configuration")
	}
	for _, port := range config.AllowedPorts {
		if port < 1 || port > 65535 {
			return nil, errors.New("invalid approved Hermes port")
		}
	}
	caPEM, err := os.ReadFile(config.CAFile)
	if err != nil {
		return nil, errors.New("Hermes resolver CA unavailable")
	}
	roots := x509.NewCertPool()
	if !roots.AppendCertsFromPEM(caPEM) {
		return nil, errors.New("invalid Hermes resolver CA")
	}
	certificate, err := tls.LoadX509KeyPair(config.CertFile, config.KeyFile)
	if err != nil {
		return nil, errors.New("Hermes resolver client certificate unavailable")
	}
	dialer := &net.Dialer{Timeout: time.Second, KeepAlive: 30 * time.Second}
	transport := &http.Transport{
		TLSClientConfig: &tls.Config{RootCAs: roots, Certificates: []tls.Certificate{certificate}, MinVersion: tls.VersionTLS12},
		// Use a closure: Yaegi does not bind this native method value correctly.
		DialContext: func(ctx context.Context, network, address string) (net.Conn, error) {
			return dialer.DialContext(ctx, network, address)
		},
		TLSHandshakeTimeout:    time.Second,
		ResponseHeaderTimeout:  2 * time.Second,
		MaxResponseHeaderBytes: maxBody,
		MaxIdleConns:           32,
		MaxIdleConnsPerHost:    16,
		MaxConnsPerHost:        64,
		IdleConnTimeout:        30 * time.Second,
	}
	client := &http.Client{Transport: transport, Timeout: 2 * time.Second, CheckRedirect: func(_ *http.Request, _ []*http.Request) error { return http.ErrUseLastResponse }}
	copied := *config
	copied.AllowedPorts = append([]int(nil), config.AllowedPorts...)
	return &middleware{next: next, client: client, config: copied}, nil
}

func validEndpoint(value, path string) bool {
	parsed, err := url.Parse(value)
	return err == nil && parsed.Scheme == "https" && parsed.Hostname() != "" && parsed.User == nil &&
		parsed.Path == path && parsed.RawQuery == "" && parsed.Fragment == "" && parsed.RawPath == ""
}

func validIssuer(value string) bool {
	parsed, err := url.Parse(value)
	return err == nil && len(value) <= 2048 && parsed.Scheme == "https" && parsed.Hostname() != "" &&
		parsed.User == nil && parsed.RawQuery == "" && parsed.Fragment == "" && strings.TrimSpace(value) == value
}

func validSubject(value string) bool {
	if len(value) < 1 || len(value) > 255 {
		return false
	}
	for _, char := range []byte(value) {
		if char < 33 || char > 126 {
			return false
		}
	}
	return true
}

func singleHeader(header http.Header, name string) string {
	values := header.Values(name)
	if len(values) != 1 {
		return ""
	}
	return values[0]
}

func (m *middleware) ServeHTTP(writer http.ResponseWriter, request *http.Request) {
	request.Header.Del(instanceHeader)
	request.Header.Del(generationHeader)
	owner := identity{Issuer: singleHeader(request.Header, issuerHeader), Subject: singleHeader(request.Header, subjectHeader)}
	token := singleHeader(request.Header, evidenceHeader)
	session := singleHeader(request.Header, sessionHeader)
	// These values must originate at the preceding authenticated ForwardAuth
	// step. The manager independently verifies the relayed evidence.
	request.Header.Del(issuerHeader)
	request.Header.Del(subjectHeader)
	request.Header.Del(evidenceHeader)
	request.Header.Del(sessionHeader)
	writer.Header().Set("Cache-Control", "no-store")
	if owner.Issuer != m.config.Issuer || !validSubject(owner.Subject) || token == "" || len(token) > maxBody {
		http.Error(writer, "invalid identity evidence", http.StatusUnauthorized)
		return
	}
	websocket := strings.EqualFold(request.Header.Get("Upgrade"), "websocket")
	if websocket && !sessionPattern.MatchString(session) {
		http.Error(writer, "invalid session evidence", http.StatusUnauthorized)
		return
	}
	resolved, status := m.resolve(request.Context(), owner, token)
	if status != http.StatusOK {
		// Never reflect a manager response body or upstream error to a browser.
		http.Error(writer, http.StatusText(status), status)
		return
	}
	request.Header.Set(instanceHeader, resolved.InstanceID)
	request.Header.Set(generationHeader, strconv.FormatInt(resolved.RouteGeneration, 10))
	if request.Header.Get("Authorization") == "Bearer "+token {
		request.Header.Del("Authorization")
	}
	if !websocket {
		m.next.ServeHTTP(writer, request)
		return
	}
	ctx, cancel := context.WithCancel(request.Context())
	defer cancel()
	guarded := &upgradeWriter{ResponseWriter: writer, onHijack: func(connection net.Conn) {
		go m.monitor(ctx, connection, owner, session, resolved)
	}}
	m.next.ServeHTTP(guarded, request)
}

func (m *middleware) post(ctx context.Context, endpoint string, body interface{}, token string, output interface{}) (int, error) {
	encoded, err := json.Marshal(body)
	if err != nil || len(encoded) > maxBody {
		return 0, errors.New("invalid internal request")
	}
	request, err := http.NewRequestWithContext(ctx, http.MethodPost, endpoint, bytes.NewReader(encoded))
	if err != nil {
		return 0, errors.New("invalid internal endpoint")
	}
	request.Header.Set("Content-Type", "application/json")
	if token != "" {
		request.Header.Set("Authorization", "Bearer "+token)
	}
	response, err := m.client.Do(request)
	if err != nil {
		return 0, errors.New("internal authorization unavailable")
	}
	defer response.Body.Close()
	if response.Header.Get("Cache-Control") != "no-store" {
		return 0, errors.New("invalid internal cache policy")
	}
	data, err := io.ReadAll(io.LimitReader(response.Body, maxBody+1))
	if err != nil || len(data) > maxBody {
		return 0, errors.New("invalid internal response size")
	}
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.DisallowUnknownFields()
	if decoder.Decode(output) != nil {
		return 0, errors.New("invalid internal response")
	}
	if decoder.Decode(new(interface{})) != io.EOF {
		return 0, errors.New("trailing internal response")
	}
	return response.StatusCode, nil
}

func (m *middleware) resolve(ctx context.Context, owner identity, token string) (decision, int) {
	var result decision
	status, err := m.post(ctx, m.config.ManagerURL, struct {
		Identity identity `json:"identity"`
	}{Identity: owner}, token, &result)
	if err != nil {
		return decision{}, http.StatusServiceUnavailable
	}
	if status != http.StatusOK {
		// Every non-ready response must contain no destination or routing data.
		if result.Destination != nil || result.InstanceID != "" || result.RouteGeneration != 0 || result.AuthorizationVersion != 0 {
			return decision{}, http.StatusServiceUnavailable
		}
		switch {
		case status == 202 && result.Status == "pending" && result.Reason == "instance_not_ready":
			return decision{}, http.StatusAccepted
		case status == 401 && result.Error == "invalid_identity_evidence":
			return decision{}, http.StatusUnauthorized
		case status == 403 && result.Status == "denied" && result.Reason == "access_denied":
			return decision{}, http.StatusForbidden
		default:
			return decision{}, http.StatusServiceUnavailable
		}
	}
	target := result.Destination
	expectedNamespace := "hi-" + m.config.InstallationPrefix + "-" + strings.ReplaceAll(result.InstanceID, "-", "")
	if result.Status != "ready" || result.Reason != "" || result.Error != "" || !uuidPattern.MatchString(result.InstanceID) || result.InstanceID == "00000000-0000-0000-0000-000000000000" ||
		result.RouteGeneration < 1 || result.AuthorizationVersion < 1 || target == nil || target.Namespace != expectedNamespace ||
		len(target.Service) > 63 || !servicePattern.MatchString(target.Service) || (target.Scheme != "http" && target.Scheme != "https") {
		return decision{}, http.StatusServiceUnavailable
	}
	allowed := false
	for _, port := range m.config.AllowedPorts {
		if port == target.Port {
			allowed = true
		}
	}
	if !allowed {
		return decision{}, http.StatusServiceUnavailable
	}
	return result, http.StatusOK
}

// A WebSocket upgrade is checked once by ServeHTTP. An established connection
// additionally reauthorizes its server-side session and live assignment every
// five seconds. A full cycle has a four-second deadline: outage/revocation must
// close the downstream within ten seconds, including two internal HTTP calls.
func (m *middleware) monitor(ctx context.Context, connection net.Conn, owner identity, session string, original decision) {
	ticker := time.NewTicker(time.Duration(m.config.RecheckIntervalMs) * time.Millisecond)
	defer ticker.Stop()
	defer connection.Close()
	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			cycle, cancel := context.WithTimeout(ctx, 4*time.Second)
			var refreshed refreshedEvidence
			status, err := m.post(cycle, m.config.SessionURL, struct {
				SessionID string `json:"sessionId"`
			}{SessionID: session}, "", &refreshed)
			if err != nil || status != http.StatusOK || refreshed.Identity != owner || refreshed.AccessToken == "" || len(refreshed.AccessToken) > maxBody {
				cancel()
				return
			}
			current, status := m.resolve(cycle, owner, refreshed.AccessToken)
			cancel()
			if status != http.StatusOK || current.InstanceID != original.InstanceID || current.RouteGeneration != original.RouteGeneration || current.AuthorizationVersion != original.AuthorizationVersion {
				return
			}
		}
	}
}

type upgradeWriter struct {
	http.ResponseWriter
	onHijack func(net.Conn)
}

func (writer *upgradeWriter) Flush() {
	if flusher, ok := writer.ResponseWriter.(http.Flusher); ok {
		flusher.Flush()
	}
}

func (writer *upgradeWriter) Unwrap() http.ResponseWriter { return writer.ResponseWriter }

func (writer *upgradeWriter) Hijack() (net.Conn, *bufio.ReadWriter, error) {
	hijacker, ok := writer.ResponseWriter.(http.Hijacker)
	if !ok {
		return nil, nil, errors.New("websocket hijacking unavailable")
	}
	connection, buffer, err := hijacker.Hijack()
	if err != nil {
		return nil, nil, err
	}
	tracked := &trackedConnection{Conn: connection, closed: make(chan struct{})}
	// Preserve already-buffered request bytes while observing proxy closure.
	wrapped := bufio.NewReadWriter(buffer.Reader, bufio.NewWriter(tracked))
	writer.onHijack(tracked)
	return tracked, wrapped, nil
}

type trackedConnection struct {
	net.Conn
	once   sync.Once
	closed chan struct{}
}

func (connection *trackedConnection) Close() error {
	var err error
	connection.once.Do(func() { close(connection.closed); err = connection.Conn.Close() })
	return err
}
