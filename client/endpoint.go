package main

import (
	"net/url"
	"strings"
)

const legacyLocalServerURL = "http://127.0.0.1:17777"

// normalizeServerURL accepts the deliberately small set of API endpoints the
// helper can use. Remote endpoints must be HTTPS; plaintext is reserved for
// local development only.
func normalizeServerURL(raw string) (string, bool) {
	value := strings.TrimSpace(raw)
	if value == "" {
		return "", false
	}
	u, err := url.ParseRequestURI(value)
	if err != nil || u.Host == "" || u.User != nil || u.RawQuery != "" || u.Fragment != "" {
		return "", false
	}
	switch strings.ToLower(u.Scheme) {
	case "https":
		return u.String(), true
	case "http":
		host := strings.ToLower(u.Hostname())
		if host == "127.0.0.1" || host == "localhost" {
			return u.String(), true
		}
	}
	return "", false
}

func isKnownLegacyLocalServerURL(value string) bool {
	normalized, ok := normalizeServerURL(value)
	return ok && normalized == legacyLocalServerURL
}

// resolveServerURL makes persisted configuration authoritative, except for
// the one legacy local default which must migrate when a remote build supplies
// a new compiled default.
func resolveServerURL(persisted, compiled string) (string, bool) {
	compiled, compiledOK := normalizeServerURL(compiled)
	if !compiledOK {
		compiled = legacyLocalServerURL
	}
	persistedNormalized, persistedOK := normalizeServerURL(persisted)
	if !persistedOK {
		return compiled, true
	}
	if !isKnownLegacyLocalServerURL(compiled) && isKnownLegacyLocalServerURL(persistedNormalized) {
		return compiled, true
	}
	return persistedNormalized, persistedNormalized != persisted
}
