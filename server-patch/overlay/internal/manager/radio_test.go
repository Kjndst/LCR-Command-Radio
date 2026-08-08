package manager

import (
	"testing"

	"github.com/disgoorg/snowflake/v2"
	"github.com/sealbro/go-discord-caller/internal/radio"
)

func TestCommandRadioAllowRequiresUnitLeaderAndOpenGate(t *testing.T) {
	previous := commandRadioRegistry
	registry := radio.NewRegistryWithSecret([]byte("test-secret-32-bytes-minimum-123456"))
	commandRadioRegistry = registry
	t.Cleanup(func() { commandRadioRegistry = previous })

	guildID := snowflake.ID(1)
	unitLeaderID := snowflake.ID(2)
	code, err := registry.IssuePairCode(guildID.String(), unitLeaderID.String())
	if err != nil {
		t.Fatal(err)
	}
	token, _, err := registry.Pair(code, "Unit Leader PC")
	if err != nil {
		t.Fatal(err)
	}

	var svc Service
	allowUnitLeader := svc.commandRadioAllow(guildID, func(userID snowflake.ID) bool { return userID == unitLeaderID })
	if allowUnitLeader(unitLeaderID) {
		t.Fatal("closed command gate must fail closed")
	}
	if _, err := registry.SetState(token, true); err != nil {
		t.Fatal(err)
	}
	if !allowUnitLeader(unitLeaderID) {
		t.Fatal("open gate must admit the Unit Leader")
	}
	if svc.commandRadioAllow(guildID, func(snowflake.ID) bool { return false })(unitLeaderID) {
		t.Fatal("open gate must not admit a user without the Unit Leader role")
	}
}

func TestCommandRadioCommanderDoesNotNeedUnitLeaderRole(t *testing.T) {
	if !commandRadioCommanderAllowed(false, true) {
		t.Fatal("Commander role should be sufficient for Command-channel capture")
	}
	if commandRadioCommanderAllowed(false, false) {
		t.Fatal("non-Commander must not be captured in the Command channel")
	}
	if commandRadioCommanderAllowed(true, true) {
		t.Fatal("bot accounts must not be captured")
	}
}
