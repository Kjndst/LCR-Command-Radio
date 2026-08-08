package bot

import (
	"encoding/json"
	"strings"
	"testing"

	"github.com/disgoorg/disgo/discord"
	"github.com/disgoorg/snowflake/v2"
	"github.com/sealbro/go-discord-caller/internal/guild"
	"github.com/sealbro/go-discord-caller/internal/i18n"
	"github.com/sealbro/go-discord-caller/internal/manager"
	"github.com/sealbro/go-discord-caller/internal/store"
)

func TestSpeakerAddedContinuationUsesExistingSetupRoutes(t *testing.T) {
	bundle, err := i18n.NewBundle()
	if err != nil {
		t.Fatal(err)
	}
	loc := bundle.For("", "")
	content, components := (&CommandHandlers{}).buildSpeakerAddedMessage(loc)
	if content != loc.T("speaker.added_title") {
		t.Fatalf("continuation content = %q, want success message", content)
	}

	payload, err := json.Marshal(components)
	if err != nil {
		t.Fatalf("marshal continuation components: %v", err)
	}
	for _, want := range []string{
		loc.T("btn.add_another_speaker"),
		`"custom_id":"/speakers/add"`,
		loc.T("btn.done_back_setup"),
		`"custom_id":"/speakers/menu"`,
	} {
		if !strings.Contains(string(payload), want) {
			t.Errorf("continuation components missing %q: %s", want, payload)
		}
	}
}

type roleBindingCall struct {
	roleType store.RoleType
	roleID   snowflake.ID
}

func TestCommandRadioSetupOptionsPreserveLegacyStartModes(t *testing.T) {
	bundle, err := i18n.NewBundle()
	if err != nil {
		t.Fatal(err)
	}

	var setupOptions []discord.ApplicationCommandOption
	var modes map[string]bool
	for _, command := range BuildCommands(bundle) {
		slash, ok := command.(discord.SlashCommandCreate)
		if !ok {
			continue
		}
		switch slash.Name {
		case "setup":
			setupOptions = slash.Options
		case "start":
			for _, option := range slash.Options {
				stringOption, ok := option.(discord.ApplicationCommandOptionString)
				if !ok || stringOption.Name != "mode" {
					continue
				}
				modes = make(map[string]bool, len(stringOption.Choices))
				for _, choice := range stringOption.Choices {
					modes[choice.Value] = true
				}
			}
		}
	}

	roles := map[string]bool{}
	for _, option := range setupOptions {
		roleOption, ok := option.(discord.ApplicationCommandOptionRole)
		if ok && !roleOption.Required {
			roles[roleOption.Name] = true
		}
	}
	if !roles[setupCommanderRoleOption] || !roles[setupUnitLeaderRoleOption] {
		t.Fatalf("/setup optional role options = %#v", roles)
	}
	for _, legacyMode := range []string{callerModeOne, callerModeMany, callerModeOneMany} {
		if !modes[legacyMode] {
			t.Fatalf("legacy /start mode %q missing from %#v", legacyMode, modes)
		}
	}
	if !modes[callerModeCommand] {
		t.Fatalf("command /start mode missing from %#v", modes)
	}
}

type recordingRoleBinder struct{ calls []roleBindingCall }

type commanderAuthorizationManager struct {
	*manager.Service
	hasCommanderRole bool
}

func (m commanderAuthorizationManager) HasManagerRole(snowflake.ID, []snowflake.ID) bool {
	return m.hasCommanderRole
}

func (b *recordingRoleBinder) BindRole(_ snowflake.ID, roleType store.RoleType, roleID snowflake.ID) {
	b.calls = append(b.calls, roleBindingCall{roleType: roleType, roleID: roleID})
}

func TestBindCommandRadioSetupRoles(t *testing.T) {
	guildID := snowflake.ID(1)
	commander := snowflake.ID(2)
	unitLeader := snowflake.ID(3)

	tests := []struct {
		name                  string
		commander, unitLeader *snowflake.ID
		want                  []roleBindingCall
	}{
		{"commander role", &commander, nil, []roleBindingCall{{store.RoleTypeManager, commander}}},
		{"unit leader role", nil, &unitLeader, []roleBindingCall{{store.RoleTypeCaller, unitLeader}}},
		{"both roles", &commander, &unitLeader, []roleBindingCall{{store.RoleTypeManager, commander}, {store.RoleTypeCaller, unitLeader}}},
		{"no options", nil, nil, nil},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			binder := &recordingRoleBinder{}
			bindCommandRadioSetupRoles(binder, guildID, tt.commander, tt.unitLeader)
			if len(binder.calls) != len(tt.want) {
				t.Fatalf("bindings = %#v, want %#v", binder.calls, tt.want)
			}
			for i := range tt.want {
				if binder.calls[i] != tt.want[i] {
					t.Fatalf("binding[%d] = %#v, want %#v", i, binder.calls[i], tt.want[i])
				}
			}
		})
	}
}

func TestSetupAuthorizationStillUsesCommanderRole(t *testing.T) {
	member := &discord.ResolvedMember{Member: discord.Member{User: discord.User{ID: 1}, RoleIDs: []snowflake.ID{2}}}
	allowed := &CommandHandlers{manager: commanderAuthorizationManager{hasCommanderRole: true}}
	if !allowed.isAdminAuthorized(1, member) {
		t.Fatal("configured Commander role should retain /setup authorization")
	}

	denied := &CommandHandlers{manager: commanderAuthorizationManager{hasCommanderRole: false}}
	if denied.isAdminAuthorized(1, member) {
		t.Fatal("member without Administrator permission or Commander role must not access /setup")
	}
}

func TestResolveStartModeCommandIsLocalDefaultWithoutChangingRelayJoins(t *testing.T) {
	for _, tt := range []struct {
		name         string
		hasRelayCode bool
		mode         string
		want         guild.RaidMode
	}{
		{"local default", false, "", guild.RaidModeCommandGuildCaller},
		{"local command", false, callerModeCommand, guild.RaidModeCommandGuildCaller},
		{"local one", false, callerModeOne, guild.RaidModeOneCaller},
		{"local many", false, callerModeMany, guild.RaidModeGuildCaller},
		{"local one-many", false, callerModeOneMany, guild.RaidModeOneManyGuildCaller},
		{"relay default remains listener", true, "", guild.RaidModeAllyListener},
		{"relay one remains listener", true, callerModeOne, guild.RaidModeAllyListener},
		{"relay many", true, callerModeMany, guild.RaidModeAllyCaller},
		{"relay one-many", true, callerModeOneMany, guild.RaidModeOneManyAllyCaller},
		{"relay command remains listener", true, callerModeCommand, guild.RaidModeAllyListener},
	} {
		t.Run(tt.name, func(t *testing.T) {
			if got := resolveStartMode(tt.hasRelayCode, tt.mode); got != tt.want {
				t.Fatalf("resolveStartMode(%v, %q) = %q, want %q", tt.hasRelayCode, tt.mode, got, tt.want)
			}
		})
	}
}
