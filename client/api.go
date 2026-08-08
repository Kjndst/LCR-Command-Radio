package main

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"strings"
	"time"
)

type RadioAPI struct {
	base   string
	token  string
	client *http.Client
}

type PairResult struct {
	Token         string `json:"token"`
	GuildID       string `json:"guild_id"`
	UserID        string `json:"user_id"`
	HeartbeatMS   int    `json:"heartbeat_ms"`
	GateTimeoutMS int    `json:"gate_timeout_ms"`
}

func NewRadioAPI(base, token string) *RadioAPI {
	return &RadioAPI{
		base:   strings.TrimRight(base, "/"),
		token:  token,
		client: &http.Client{Timeout: 2200 * time.Millisecond},
	}
}

func (a *RadioAPI) SetToken(token string) { a.token = token }

func (a *RadioAPI) Pair(ctx context.Context, code, device string) (PairResult, error) {
	var out PairResult
	err := a.doJSON(ctx, http.MethodPost, "/v1/pair", map[string]any{"code": strings.TrimSpace(code), "device": device}, false, &out)
	return out, err
}

func (a *RadioAPI) SetState(ctx context.Context, transmitting bool) error {
	return a.doJSON(ctx, http.MethodPost, "/v1/state", map[string]bool{"transmitting": transmitting}, true, nil)
}

func (a *RadioAPI) Heartbeat(ctx context.Context) error {
	return a.doJSON(ctx, http.MethodPost, "/v1/heartbeat", map[string]any{}, true, nil)
}

func (a *RadioAPI) Probe(ctx context.Context) error {
	return a.doJSON(ctx, http.MethodGet, "/v1/status", nil, true, nil)
}

func (a *RadioAPI) doJSON(ctx context.Context, method, path string, payload any, auth bool, out any) error {
	var body io.Reader
	if payload != nil {
		b, err := json.Marshal(payload)
		if err != nil {
			return err
		}
		body = bytes.NewReader(b)
	}
	req, err := http.NewRequestWithContext(ctx, method, a.base+path, body)
	if err != nil {
		return err
	}
	if payload != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	if auth {
		if a.token == "" {
			return fmt.Errorf("not paired")
		}
		req.Header.Set("Authorization", "Bearer "+a.token)
	}
	resp, err := a.client.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		b, _ := io.ReadAll(io.LimitReader(resp.Body, 1024))
		return fmt.Errorf("server returned %s: %s", resp.Status, strings.TrimSpace(string(b)))
	}
	if out != nil {
		if err := json.NewDecoder(resp.Body).Decode(out); err != nil {
			return err
		}
	}
	return nil
}
