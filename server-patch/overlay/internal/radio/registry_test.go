package radio

import (
	"bytes"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"
)

func newTestRegistry() *Registry {
	r := NewRegistryWithSecret([]byte("test-secret-32-bytes-minimum-123456"))
	now := time.Unix(1_700_000_000, 0)
	r.now = func() time.Time { return now }
	return r
}

func TestPairIsOneTimeAndGateDefaultsClosed(t *testing.T) {
	r := newTestRegistry()
	code, err := r.IssuePairCode("guild-1", "user-1")
	if err != nil {
		t.Fatal(err)
	}
	token, claims, err := r.Pair(code, "Lead PC")
	if err != nil {
		t.Fatal(err)
	}
	if token == "" || claims.UserID != "user-1" {
		t.Fatalf("bad pair result: %#v", claims)
	}
	if _, _, err := r.Pair(code, "again"); err != ErrInvalidPairCode {
		t.Fatalf("expected one-time code rejection, got %v", err)
	}
	if r.IsOpen("guild-1", "user-1") {
		t.Fatal("gate must default closed")
	}
}

func TestOpenHeartbeatCloseAndFailClosedTimeout(t *testing.T) {
	r := newTestRegistry()
	now := time.Unix(1_700_000_000, 0)
	r.now = func() time.Time { return now }
	code, _ := r.IssuePairCode("g", "u")
	token, _, _ := r.Pair(code, "pc")

	if _, err := r.SetState(token, true); err != nil {
		t.Fatal(err)
	}
	if !r.IsOpen("g", "u") {
		t.Fatal("gate should be open")
	}

	now = now.Add(500 * time.Millisecond)
	if _, err := r.Heartbeat(token); err != nil {
		t.Fatal(err)
	}
	now = now.Add(800 * time.Millisecond)
	if !r.IsOpen("g", "u") {
		t.Fatal("fresh heartbeat should keep gate open")
	}

	now = now.Add(901 * time.Millisecond)
	if r.IsOpen("g", "u") {
		t.Fatal("stale gate must fail closed")
	}

	if _, err := r.SetState(token, true); err != nil {
		t.Fatal(err)
	}
	if _, err := r.SetState(token, false); err != nil {
		t.Fatal(err)
	}
	if r.IsOpen("g", "u") {
		t.Fatal("explicit close failed")
	}
}

func TestTamperedTokenRejected(t *testing.T) {
	r := newTestRegistry()
	code, _ := r.IssuePairCode("g", "u")
	token, _, _ := r.Pair(code, "pc")
	token = token[:len(token)-1] + "A"
	if _, err := r.SetState(token, true); err == nil {
		t.Fatal("tampered token accepted")
	}
}

func TestHTTPPairAndStateFlow(t *testing.T) {
	r := newTestRegistry()
	code, _ := r.IssuePairCode("guild-x", "user-x")
	srv := httptest.NewServer((&HTTPServer{Registry: r}).Handler())
	defer srv.Close()

	pairPayload, _ := json.Marshal(map[string]string{"code": code, "device": "test"})
	resp, err := http.Post(srv.URL+"/v1/pair", "application/json", bytes.NewReader(pairPayload))
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("pair status=%d", resp.StatusCode)
	}
	var pairResp struct {
		Token string `json:"token"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&pairResp); err != nil {
		t.Fatal(err)
	}

	statePayload := bytes.NewBufferString(`{"transmitting":true}`)
	req, _ := http.NewRequest(http.MethodPost, srv.URL+"/v1/state", statePayload)
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("Authorization", "Bearer "+pairResp.Token)
	resp2, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	resp2.Body.Close()
	if resp2.StatusCode != http.StatusOK {
		t.Fatalf("state status=%d", resp2.StatusCode)
	}
	if !r.IsOpen("guild-x", "user-x") {
		t.Fatal("HTTP state did not open gate")
	}
}
