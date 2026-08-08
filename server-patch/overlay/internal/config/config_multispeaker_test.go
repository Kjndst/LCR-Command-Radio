package config

import "testing"

func TestLoadSpeakerTokensSupportsThreeOrderedIdentities(t *testing.T) {
	// High indices avoid colliding with a developer's normal numbered pool.
	t.Setenv("DISCORD_SPEAKER_BOT_TOKEN_901", "speaker-one")
	t.Setenv("DISCORD_SPEAKER_BOT_TOKEN_902", "speaker-two")
	t.Setenv("DISCORD_SPEAKER_BOT_TOKEN_903", "speaker-three")

	got := loadSpeakerTokens()
	positions := map[string]int{}
	for i, token := range got {
		positions[token] = i
	}
	first, ok1 := positions["speaker-one"]
	second, ok2 := positions["speaker-two"]
	third, ok3 := positions["speaker-three"]
	if !ok1 || !ok2 || !ok3 {
		t.Fatalf("three configured speaker identities were not loaded: %#v", got)
	}
	if !(first < second && second < third) {
		t.Fatalf("speaker identities are not ordered by numeric suffix: %#v", got)
	}
}
