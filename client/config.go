package main

import (
	"encoding/json"
	"os"
	"path/filepath"
)

type AppConfig struct {
	ServerURL   string `json:"server_url"`
	DeviceToken string `json:"device_token"`
	GuildID     string `json:"guild_id"`
	UserID      string `json:"user_id"`
	RadioKey    string `json:"radio_key"`
	RadioMode   string `json:"radio_mode"`
	VoiceMode   string `json:"voice_mode"`
}

func defaultConfig() AppConfig {
	return AppConfig{
		ServerURL: defaultServerURL,
		RadioKey:  "Mouse5",
		RadioMode: "Hold",
		VoiceMode: "OpenMic",
	}
}

func configPath() string {
	base, err := os.UserConfigDir()
	if err != nil || base == "" {
		base = os.Getenv("APPDATA")
	}
	return filepath.Join(base, "LLBCommandRadio", "config.json")
}

func loadConfig() AppConfig {
	cfg := defaultConfig()
	b, err := os.ReadFile(configPath())
	if err != nil {
		return cfg
	}
	_ = json.Unmarshal(b, &cfg)
	if cfg.ServerURL == "" {
		cfg.ServerURL = defaultServerURL
	}
	if cfg.RadioKey == "" {
		cfg.RadioKey = "Mouse5"
	}
	if cfg.RadioMode == "" {
		cfg.RadioMode = "Hold"
	}
	if cfg.VoiceMode == "" {
		cfg.VoiceMode = "OpenMic"
	}
	return cfg
}

func saveConfig(cfg AppConfig) error {
	p := configPath()
	if err := os.MkdirAll(filepath.Dir(p), 0o700); err != nil {
		return err
	}
	b, err := json.MarshalIndent(cfg, "", "  ")
	if err != nil {
		return err
	}
	return os.WriteFile(p, b, 0o600)
}
