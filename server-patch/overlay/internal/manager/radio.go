package manager

import (
	"context"
	"os"

	"github.com/disgoorg/snowflake/v2"
	"github.com/sealbro/go-discord-caller/internal/radio"
)

// commandRadioRegistry is process-scoped. Device tokens remain valid across
// restarts only when LLB_RADIO_SECRET is configured to a stable value.
var commandRadioRegistry = radio.NewRegistry()

// IssueRadioPairCode returns a one-time pairing code tied to one Discord user
// in one guild. The code expires quickly and can be redeemed only once.
func (m *Service) IssueRadioPairCode(guildID, userID snowflake.ID) (string, error) {
	return commandRadioRegistry.IssuePairCode(guildID.String(), userID.String())
}

// commandRadioAllow composes the existing capture-role filter with the
// per-user radio gate. It is used only for speaker-room receivers in command
// mode; the owner/shotcaller receiver continues to use the normal role filter.
func (m *Service) commandRadioAllow(guildID snowflake.ID, base func(snowflake.ID) bool) func(snowflake.ID) bool {
	return func(userID snowflake.ID) bool {
		if !base(userID) {
			return false
		}
		return commandRadioRegistry.IsOpen(guildID.String(), userID.String())
	}
}

// StartRadioAPI serves the tiny helper-control API. For production expose this
// through HTTPS (reverse proxy / tunnel); helpers should not send bearer tokens
// over plaintext internet links.
func (m *Service) StartRadioAPI(ctx context.Context) error {
	return (&radio.HTTPServer{
		Registry: commandRadioRegistry,
		Addr:     os.Getenv("LLB_RADIO_ADDR"),
	}).Run(ctx)
}
