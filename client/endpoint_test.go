package main

import "testing"

func TestResolveServerURLPrecedenceAndMigration(t *testing.T) {
	remote := "https://example.trycloudflare.com"
	for _, tc := range []struct {
		name, persisted, want string
		changed               bool
	}{
		{"fresh uses compiled default", "", remote, true},
		{"known localhost migrates", legacyLocalServerURL, remote, true},
		{"custom remote remains", "https://custom.example.net", "https://custom.example.net", false},
		{"invalid falls back", "ftp://example.net", remote, true},
		{"trimmed remote persists normalized", " https://custom.example.net ", "https://custom.example.net", true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			got, changed := resolveServerURL(tc.persisted, remote)
			if got != tc.want || changed != tc.changed {
				t.Fatalf("resolveServerURL(%q) = (%q, %t), want (%q, %t)", tc.persisted, got, changed, tc.want, tc.changed)
			}
		})
	}
}

func TestNormalizeServerURL(t *testing.T) {
	for _, tc := range []struct {
		value string
		ok    bool
	}{
		{"https://example.trycloudflare.com", true},
		{"http://127.0.0.1:17777", true},
		{"http://localhost:17777", true},
		{"file:///tmp/x", false},
		{"javascript:alert(1)", false},
		{"ftp://example.net", false},
		{"https://example.net/path?q=1", false},
		{"not a URL", false},
	} {
		_, ok := normalizeServerURL(tc.value)
		if ok != tc.ok {
			t.Errorf("normalizeServerURL(%q) ok = %t, want %t", tc.value, ok, tc.ok)
		}
	}
}

func TestLoadConfigPersistsEffectiveServerURL(t *testing.T) {
	t.Setenv("XDG_CONFIG_HOME", t.TempDir())
	oldDefault := defaultServerURL
	defaultServerURL = "https://default.example.net"
	t.Cleanup(func() { defaultServerURL = oldDefault })
	got := loadConfig()
	if got.ServerURL != defaultServerURL {
		t.Fatalf("fresh server URL = %q, want %q", got.ServerURL, defaultServerURL)
	}
	got.ServerURL = legacyLocalServerURL
	got.DeviceToken = "preserved-test-token"
	got.GuildID = "guild"
	if err := saveConfig(got); err != nil {
		t.Fatal(err)
	}
	got = loadConfig()
	if got.ServerURL != defaultServerURL || got.DeviceToken != "preserved-test-token" || got.GuildID != "guild" {
		t.Fatalf("migration did not preserve unrelated config: %#v", got)
	}
}
