package bot

import (
	"github.com/disgoorg/disgo/discord"
	"github.com/disgoorg/disgo/handler"
	"github.com/disgoorg/snowflake/v2"
	"github.com/sealbro/go-discord-caller/internal/i18n"
)

// handleRadioPair lets a capture-role member pair a Windows helper without
// exposing any Discord credential or user token to the helper.
func (h *CommandHandlers) handleRadioPair(guildID snowflake.ID, loc *i18n.Localizer, _ discord.SlashCommandInteractionData, e *handler.CommandEvent) error {
	member := e.Member()
	if member == nil || !h.manager.HasCallerRole(guildID, member.Member.RoleIDs) {
		return e.CreateMessage(ephemeral(loc.T("radio.need_caller")))
	}

	code, err := h.manager.IssueRadioPairCode(guildID, e.User().ID)
	if err != nil {
		return e.CreateMessage(ephemeral(loc.T("radio.pair_failed", "Err", err.Error())))
	}
	return e.CreateMessage(ephemeral(loc.T("radio.pair_code", "Code", code)))
}
