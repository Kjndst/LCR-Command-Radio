package radio

import (
	"crypto/hmac"
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"strings"
	"sync"
	"time"
)

const (
	DefaultPairTTL       = 5 * time.Minute
	DefaultOpenTimeout   = 900 * time.Millisecond
	DefaultHeartbeatHint = 250 * time.Millisecond
)

var (
	ErrInvalidPairCode = errors.New("invalid or expired pair code")
	ErrInvalidToken    = errors.New("invalid device token")
)

type PairRequest struct {
	GuildID   string
	UserID    string
	ExpiresAt time.Time
}

type Claims struct {
	GuildID string `json:"guild_id"`
	UserID  string `json:"user_id"`
	Device  string `json:"device"`
	Issued  int64  `json:"issued"`
	Nonce   string `json:"nonce"`
}

type gateState struct {
	Open     bool
	LastSeen time.Time
}

type Registry struct {
	mu          sync.RWMutex
	secret      []byte
	pending     map[string]PairRequest
	gates       map[string]gateState
	pairTTL     time.Duration
	openTimeout time.Duration
	now         func() time.Time
}

func NewRegistry() *Registry {
	secret := strings.TrimSpace(os.Getenv("LLB_RADIO_SECRET"))
	if secret == "" {
		raw := make([]byte, 32)
		if _, err := rand.Read(raw); err != nil {
			panic(fmt.Errorf("radio: generate signing secret: %w", err))
		}
		secret = base64.RawURLEncoding.EncodeToString(raw)
	}
	return NewRegistryWithSecret([]byte(secret))
}

func NewRegistryWithSecret(secret []byte) *Registry {
	cp := append([]byte(nil), secret...)
	return &Registry{
		secret:      cp,
		pending:     make(map[string]PairRequest),
		gates:       make(map[string]gateState),
		pairTTL:     DefaultPairTTL,
		openTimeout: DefaultOpenTimeout,
		now:         time.Now,
	}
}

func (r *Registry) IssuePairCode(guildID, userID string) (string, error) {
	if guildID == "" || userID == "" {
		return "", errors.New("guildID and userID are required")
	}
	const alphabet = "ABCDEFGHJKLMNPQRSTUVWXYZ23456789"
	raw := make([]byte, 8)
	if _, err := rand.Read(raw); err != nil {
		return "", fmt.Errorf("generate pair code: %w", err)
	}
	codeBytes := make([]byte, len(raw))
	for i, b := range raw {
		codeBytes[i] = alphabet[int(b)%len(alphabet)]
	}
	code := string(codeBytes)

	r.mu.Lock()
	defer r.mu.Unlock()
	now := r.now()
	r.prunePendingLocked(now)
	r.pending[code] = PairRequest{GuildID: guildID, UserID: userID, ExpiresAt: now.Add(r.pairTTL)}
	return code, nil
}

func (r *Registry) Pair(code, device string) (string, Claims, error) {
	code = strings.ToUpper(strings.TrimSpace(code))
	device = strings.TrimSpace(device)
	if device == "" {
		device = "Windows PC"
	}

	r.mu.Lock()
	now := r.now()
	req, ok := r.pending[code]
	if !ok || !req.ExpiresAt.After(now) {
		if ok {
			delete(r.pending, code)
		}
		r.mu.Unlock()
		return "", Claims{}, ErrInvalidPairCode
	}
	delete(r.pending, code) // one-time code
	r.mu.Unlock()

	nonceRaw := make([]byte, 12)
	if _, err := rand.Read(nonceRaw); err != nil {
		return "", Claims{}, fmt.Errorf("generate device nonce: %w", err)
	}
	claims := Claims{
		GuildID: req.GuildID,
		UserID:  req.UserID,
		Device:  device,
		Issued:  now.Unix(),
		Nonce:   base64.RawURLEncoding.EncodeToString(nonceRaw),
	}
	token, err := r.signClaims(claims)
	return token, claims, err
}

func (r *Registry) SetState(token string, transmitting bool) (Claims, error) {
	claims, err := r.Verify(token)
	if err != nil {
		return Claims{}, err
	}
	key := gateKey(claims.GuildID, claims.UserID)
	r.mu.Lock()
	defer r.mu.Unlock()
	if transmitting {
		r.gates[key] = gateState{Open: true, LastSeen: r.now()}
	} else {
		delete(r.gates, key)
	}
	return claims, nil
}

func (r *Registry) Heartbeat(token string) (Claims, error) {
	claims, err := r.Verify(token)
	if err != nil {
		return Claims{}, err
	}
	key := gateKey(claims.GuildID, claims.UserID)
	r.mu.Lock()
	defer r.mu.Unlock()
	st, ok := r.gates[key]
	if ok && st.Open {
		st.LastSeen = r.now()
		r.gates[key] = st
	}
	return claims, nil
}

// IsOpen is deliberately fail-closed. A gate is open only when the paired
// helper has explicitly opened it and a fresh heartbeat was seen recently.
func (r *Registry) IsOpen(guildID, userID string) bool {
	key := gateKey(guildID, userID)
	now := r.now()

	r.mu.RLock()
	st, ok := r.gates[key]
	timeout := r.openTimeout
	r.mu.RUnlock()
	if !ok || !st.Open {
		return false
	}
	if now.Sub(st.LastSeen) <= timeout {
		return true
	}

	// Opportunistic stale cleanup.
	r.mu.Lock()
	if current, exists := r.gates[key]; exists && now.Sub(current.LastSeen) > r.openTimeout {
		delete(r.gates, key)
	}
	r.mu.Unlock()
	return false
}

func (r *Registry) ForceClose(guildID, userID string) {
	r.mu.Lock()
	delete(r.gates, gateKey(guildID, userID))
	r.mu.Unlock()
}

func (r *Registry) Verify(token string) (Claims, error) {
	parts := strings.Split(token, ".")
	if len(parts) != 2 {
		return Claims{}, ErrInvalidToken
	}
	payload, err := base64.RawURLEncoding.DecodeString(parts[0])
	if err != nil {
		return Claims{}, ErrInvalidToken
	}
	gotSig, err := base64.RawURLEncoding.DecodeString(parts[1])
	if err != nil {
		return Claims{}, ErrInvalidToken
	}
	mac := hmac.New(sha256.New, r.secret)
	_, _ = mac.Write(payload)
	if !hmac.Equal(gotSig, mac.Sum(nil)) {
		return Claims{}, ErrInvalidToken
	}
	var claims Claims
	if err := json.Unmarshal(payload, &claims); err != nil || claims.GuildID == "" || claims.UserID == "" || claims.Nonce == "" {
		return Claims{}, ErrInvalidToken
	}
	return claims, nil
}

func (r *Registry) signClaims(claims Claims) (string, error) {
	payload, err := json.Marshal(claims)
	if err != nil {
		return "", err
	}
	mac := hmac.New(sha256.New, r.secret)
	_, _ = mac.Write(payload)
	return base64.RawURLEncoding.EncodeToString(payload) + "." + base64.RawURLEncoding.EncodeToString(mac.Sum(nil)), nil
}

func (r *Registry) prunePendingLocked(now time.Time) {
	for code, req := range r.pending {
		if !req.ExpiresAt.After(now) {
			delete(r.pending, code)
		}
	}
}

func gateKey(guildID, userID string) string { return guildID + ":" + userID }
