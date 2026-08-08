package router

import (
	"testing"

	"github.com/disgoorg/snowflake/v2"
	"github.com/sealbro/go-discord-caller/internal/opus"
)

type commandRoleProbeKey struct {
	channelID snowflake.ID
	roleID    snowflake.ID
}

type commandRoleProbe struct {
	users map[commandRoleProbeKey][]snowflake.ID
	calls []commandRoleProbeKey
}

func (p *commandRoleProbe) EnumerateCallers(channelID, roleID snowflake.ID) []snowflake.ID {
	key := commandRoleProbeKey{channelID: channelID, roleID: roleID}
	p.calls = append(p.calls, key)
	return p.users[key]
}

func (*commandRoleProbe) HasListeners(snowflake.ID) bool { return true }

func TestSourceRoleOverrideSeparatesCommanderAndUnitLeaderRouting(t *testing.T) {
	const (
		unitLeaderRole snowflake.ID = 10
		commanderRole  snowflake.ID = 11
		commandChannel snowflake.ID = 20
		unitChannel    snowflake.ID = 21
		commanderUser  snowflake.ID = 30
		unitLeaderUser snowflake.ID = 31
	)

	tests := []struct {
		name      string
		users     map[commandRoleProbeKey][]snowflake.ID
		wantOwner RouteMode
	}{
		{
			name: "Commander without Unit Leader is routed from Command Channel",
			users: map[commandRoleProbeKey][]snowflake.ID{
				{commandChannel, commanderRole}: {commanderUser},
				{unitChannel, unitLeaderRole}:   {unitLeaderUser},
			},
			wantOwner: RouteCopy,
		},
		{
			name: "Unit Leader without Commander is not routed from Command Channel",
			users: map[commandRoleProbeKey][]snowflake.ID{
				{commandChannel, unitLeaderRole}: {unitLeaderUser},
				{unitChannel, unitLeaderRole}:    {unitLeaderUser},
			},
			wantOwner: RouteOff,
		},
		{
			name: "user with both roles is routed from Command Channel",
			users: map[commandRoleProbeKey][]snowflake.ID{
				{commandChannel, commanderRole}:  {commanderUser},
				{commandChannel, unitLeaderRole}: {commanderUser},
				{unitChannel, unitLeaderRole}:    {unitLeaderUser},
			},
			wantOwner: RouteCopy,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			probe := &commandRoleProbe{users: tt.users}
			owner := &SourceSlot{
				ID:        100,
				ChannelID: commandChannel,
				RoleID:    commanderRole,
				Handle:    opus.NewFanoutHandle(),
				BuildInstall: func(RouteMode, []UserBinding) (opus.FanoutInstall, func()) {
					return opus.FanoutInstall{}, func() {}
				},
			}
			speaker := &SourceSlot{
				ID:        101,
				ChannelID: unitChannel,
				Handle:    opus.NewFanoutHandle(),
				BuildInstall: func(RouteMode, []UserBinding) (opus.FanoutInstall, func()) {
					return opus.FanoutInstall{}, func() {}
				},
			}

			r := New(1, unitLeaderRole, probe, []*SourceSlot{owner, speaker}, nil)
			r.Recompute()

			if owner.activeMode != tt.wantOwner {
				t.Fatalf("Command owner route = %s, want %s", owner.activeMode, tt.wantOwner)
			}
			if speaker.activeMode != RouteCopy {
				t.Fatalf("Unit speaker route = %s, want copy", speaker.activeMode)
			}
			if len(probe.calls) != 2 {
				t.Fatalf("role probe calls = %#v, want one per source", probe.calls)
			}
			seen := make(map[commandRoleProbeKey]bool, len(probe.calls))
			for _, call := range probe.calls {
				seen[call] = true
			}
			if !seen[commandRoleProbeKey{commandChannel, commanderRole}] {
				t.Fatalf("Command source did not query Commander role: %#v", probe.calls)
			}
			if !seen[commandRoleProbeKey{unitChannel, unitLeaderRole}] {
				t.Fatalf("Unit source did not retain Unit Leader role: %#v", probe.calls)
			}
		})
	}
}

func TestSourceWithoutOverrideKeepsLegacyCallerRole(t *testing.T) {
	const (
		callerRole snowflake.ID = 40
		channelID  snowflake.ID = 41
		callerID   snowflake.ID = 42
	)
	probe := &commandRoleProbe{users: map[commandRoleProbeKey][]snowflake.ID{
		{channelID, callerRole}: {callerID},
	}}
	source := &SourceSlot{
		ID:        102,
		ChannelID: channelID,
		Handle:    opus.NewFanoutHandle(),
		BuildInstall: func(RouteMode, []UserBinding) (opus.FanoutInstall, func()) {
			return opus.FanoutInstall{}, func() {}
		},
	}

	r := New(1, callerRole, probe, []*SourceSlot{source}, nil)
	r.Recompute()
	if source.activeMode != RouteCopy {
		t.Fatalf("legacy source route = %s, want copy", source.activeMode)
	}
	if len(probe.calls) != 1 || probe.calls[0] != (commandRoleProbeKey{channelID, callerRole}) {
		t.Fatalf("legacy source queried %#v, want caller role", probe.calls)
	}
}
