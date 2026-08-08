package bot

import (
	"github.com/disgoorg/snowflake/v2"
	"github.com/sealbro/go-discord-caller/internal/guild"
	"github.com/sealbro/go-discord-caller/internal/store"
)

const (
	setupCommanderRoleOption  = "commander-role"
	setupUnitLeaderRoleOption = "unit-leader-role"
)

// bindCommandRadioSetupRoles reuses the normal persisted role bindings. A nil
// option deliberately leaves the existing binding unchanged, so /setup without
// role options continues to open the normal setup panel unchanged.
func bindCommandRadioSetupRoles(binder interface {
	BindRole(snowflake.ID, store.RoleType, snowflake.ID)
}, guildID snowflake.ID, commanderRole, unitLeaderRole *snowflake.ID) {
	if commanderRole != nil {
		binder.BindRole(guildID, store.RoleTypeManager, *commanderRole)
	}
	if unitLeaderRole != nil {
		binder.BindRole(guildID, store.RoleTypeCaller, *unitLeaderRole)
	}
}

// resolveStartMode preserves the upstream guest mapping while making Command
// Radio the local host default for /start without a relay code or mode option.
func resolveStartMode(hasRelayCode bool, mode string) guild.RaidMode {
	if hasRelayCode {
		switch mode {
		case callerModeMany:
			return guild.RaidModeAllyCaller
		case callerModeOneMany:
			return guild.RaidModeOneManyAllyCaller
		default:
			return guild.RaidModeAllyListener
		}
	}

	switch mode {
	case callerModeOne:
		return guild.RaidModeOneCaller
	case callerModeMany:
		return guild.RaidModeGuildCaller
	case callerModeOneMany:
		return guild.RaidModeOneManyGuildCaller
	default:
		return guild.RaidModeCommandGuildCaller
	}
}
