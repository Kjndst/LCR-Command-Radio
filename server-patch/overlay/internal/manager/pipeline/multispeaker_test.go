package pipeline

import (
	"context"
	"testing"

	"github.com/disgoorg/snowflake/v2"
	"github.com/sealbro/go-discord-caller/internal/guild"
	"github.com/sealbro/go-discord-caller/internal/pool"
)

func TestBuildDestinationsKeepsThreeUnitChannelsIsolated(t *testing.T) {
	channelIDs := []snowflake.ID{1002, 1003, 1004}
	joined := make([]SpeakerResult, 0, len(channelIDs))

	for i, channelID := range channelIDs {
		joined = append(joined, SpeakerResult{
			Speaker: guild.Speaker{ID: snowflake.ID(200 + i), Enabled: true},
			ChOut:   make(chan []byte, 1),
			GV:      pool.NewGuildVoice(nil, channelID),
		})
	}

	destinations := BuildDestinations(joined)
	if len(destinations) != len(channelIDs) {
		t.Fatalf("BuildDestinations() returned %d destinations, want %d", len(destinations), len(channelIDs))
	}

	seen := make(map[snowflake.ID]bool, len(channelIDs))
	for _, destination := range destinations {
		if seen[destination.ChannelID] {
			t.Fatalf("channel %d was merged with another unit destination", destination.ChannelID)
		}
		seen[destination.ChannelID] = true
		if len(destination.Outs) != 1 {
			t.Fatalf("channel %d has %d outputs, want one isolated speaker output", destination.ChannelID, len(destination.Outs))
		}
	}

	for _, channelID := range channelIDs {
		if !seen[channelID] {
			t.Fatalf("channel %d is missing from destinations", channelID)
		}
	}
}

func TestStarCallerPipelineThreeSpeakersKeepsUnitUplinksAtCommandHub(t *testing.T) {
	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()

	fx := hostFx()
	fx.speakerIDs = append(fx.speakerIDs, 202)
	fx.speakerChIDs = append(fx.speakerChIDs, 1004)
	p := buildHostParams(t, ctx, fx, guild.RaidModeOneManyGuildCaller)

	session, start, err := HostFor(guild.RaidModeOneManyGuildCaller).Build(ctx, p)
	if err != nil {
		t.Fatalf("build: %v", err)
	}
	if len(session.ChannelMixers) != 2 {
		t.Fatalf("ChannelMixers count: got %d, want only command hub and relay", len(session.ChannelMixers))
	}
	for _, unitChannelID := range fx.speakerChIDs {
		if _, found := session.ChannelMixers[unitChannelID]; found {
			t.Fatalf("unit channel %d unexpectedly has a mixer destination", unitChannelID)
		}
	}

	start()
	t.Cleanup(func() {
		p.OwnerHandle.Close()
		for _, result := range p.Setup.Joined {
			result.Handle.Close()
		}
	})

	hubMixer := mixerOf(t, session.ChannelMixers[fx.ownerChannelID])
	synthInputs := 0
	for _, id := range hubMixer.InputIDs() {
		if uint64(id)>>63 == 1 {
			synthInputs++
		}
	}
	if synthInputs != 6 {
		t.Fatalf("command hub has %d synthetic unit inputs, want 6 (3 units x 2 callers)", synthInputs)
	}
}
