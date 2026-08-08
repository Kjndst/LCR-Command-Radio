package radio

import (
	"context"
	"encoding/json"
	"errors"
	"log/slog"
	"net/http"
	"strings"
	"time"
)

type HTTPServer struct {
	Registry *Registry
	Addr     string
}

type pairBody struct {
	Code   string `json:"code"`
	Device string `json:"device"`
}

type stateBody struct {
	Transmitting bool `json:"transmitting"`
}

func (s *HTTPServer) Handler() http.Handler {
	mux := http.NewServeMux()
	mux.HandleFunc("POST /v1/pair", s.handlePair)
	mux.HandleFunc("POST /v1/state", s.handleState)
	mux.HandleFunc("POST /v1/heartbeat", s.handleHeartbeat)
	mux.HandleFunc("GET /v1/status", s.handleStatus)
	mux.HandleFunc("GET /healthz", func(w http.ResponseWriter, _ *http.Request) { w.WriteHeader(http.StatusNoContent) })
	return securityHeaders(mux)
}

func (s *HTTPServer) Run(ctx context.Context) error {
	if s.Registry == nil {
		return errors.New("radio http: registry is nil")
	}
	addr := strings.TrimSpace(s.Addr)
	if addr == "" {
		addr = "127.0.0.1:17777"
	}
	srv := &http.Server{
		Addr:              addr,
		Handler:           s.Handler(),
		ReadHeaderTimeout: 3 * time.Second,
		ReadTimeout:       5 * time.Second,
		WriteTimeout:      5 * time.Second,
		IdleTimeout:       30 * time.Second,
	}
	errCh := make(chan error, 1)
	go func() {
		slog.Info("command radio API listening", slog.String("addr", addr))
		errCh <- srv.ListenAndServe()
	}()
	select {
	case <-ctx.Done():
		shutdownCtx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
		defer cancel()
		_ = srv.Shutdown(shutdownCtx)
		return nil
	case err := <-errCh:
		if errors.Is(err, http.ErrServerClosed) {
			return nil
		}
		return err
	}
}

func (s *HTTPServer) handlePair(w http.ResponseWriter, req *http.Request) {
	var body pairBody
	if !decodeJSON(w, req, &body) {
		return
	}
	token, claims, err := s.Registry.Pair(body.Code, body.Device)
	if err != nil {
		http.Error(w, "invalid or expired pair code", http.StatusUnauthorized)
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{
		"token":           token,
		"guild_id":        claims.GuildID,
		"user_id":         claims.UserID,
		"heartbeat_ms":    DefaultHeartbeatHint.Milliseconds(),
		"gate_timeout_ms": DefaultOpenTimeout.Milliseconds(),
	})
}

func (s *HTTPServer) handleState(w http.ResponseWriter, req *http.Request) {
	token, ok := bearer(req)
	if !ok {
		http.Error(w, "missing bearer token", http.StatusUnauthorized)
		return
	}
	var body stateBody
	if !decodeJSON(w, req, &body) {
		return
	}
	claims, err := s.Registry.SetState(token, body.Transmitting)
	if err != nil {
		http.Error(w, "invalid token", http.StatusUnauthorized)
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{
		"transmitting": body.Transmitting,
		"guild_id":     claims.GuildID,
		"user_id":      claims.UserID,
	})
}

func (s *HTTPServer) handleHeartbeat(w http.ResponseWriter, req *http.Request) {
	token, ok := bearer(req)
	if !ok {
		http.Error(w, "missing bearer token", http.StatusUnauthorized)
		return
	}
	if _, err := s.Registry.Heartbeat(token); err != nil {
		http.Error(w, "invalid token", http.StatusUnauthorized)
		return
	}
	w.WriteHeader(http.StatusNoContent)
}

func (s *HTTPServer) handleStatus(w http.ResponseWriter, req *http.Request) {
	token, ok := bearer(req)
	if !ok {
		http.Error(w, "missing bearer token", http.StatusUnauthorized)
		return
	}
	claims, err := s.Registry.Verify(token)
	if err != nil {
		http.Error(w, "invalid token", http.StatusUnauthorized)
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{
		"paired":       true,
		"transmitting": s.Registry.IsOpen(claims.GuildID, claims.UserID),
		"guild_id":     claims.GuildID,
		"user_id":      claims.UserID,
		"device":       claims.Device,
	})
}

func bearer(req *http.Request) (string, bool) {
	raw := strings.TrimSpace(req.Header.Get("Authorization"))
	const prefix = "Bearer "
	if !strings.HasPrefix(raw, prefix) {
		return "", false
	}
	token := strings.TrimSpace(strings.TrimPrefix(raw, prefix))
	return token, token != ""
}

func decodeJSON(w http.ResponseWriter, req *http.Request, dst any) bool {
	dec := json.NewDecoder(http.MaxBytesReader(w, req.Body, 8<<10))
	dec.DisallowUnknownFields()
	if err := dec.Decode(dst); err != nil {
		http.Error(w, "invalid JSON", http.StatusBadRequest)
		return false
	}
	return true
}

func writeJSON(w http.ResponseWriter, status int, v any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(v)
}

func securityHeaders(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("X-Content-Type-Options", "nosniff")
		w.Header().Set("Cache-Control", "no-store")
		next.ServeHTTP(w, r)
	})
}
