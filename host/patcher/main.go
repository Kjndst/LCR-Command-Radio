package main

import (
	"flag"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"sort"
	"strings"
)

const baseHint = "9845af2e26d7e39ad15843a674102311b1ef43df"

func must(err error) {
	if err != nil {
		panic(err)
	}
}

func read(path string) string {
	b, err := os.ReadFile(path)
	must(err)
	return string(b)
}

func write(path, s string) {
	must(os.WriteFile(path, []byte(s), 0o644))
}

func replaceOnce(path, old, neu string) {
	text := read(path)
	n := strings.Count(text, old)
	if n != 1 {
		panic(fmt.Errorf("%s: expected anchor exactly once, found %d: %q", path, n, trim(old, 100)))
	}
	write(path, strings.Replace(text, old, neu, 1))
}

func trim(s string, n int) string {
	if len(s) > n {
		return s[:n]
	}
	return s
}

func appendAfterLinePrefix(path, prefix, newLine string) {
	text := read(path)
	lines := strings.Split(strings.TrimSuffix(text, "\n"), "\n")
	idx := -1
	for i, line := range lines {
		if strings.HasPrefix(line, prefix) {
			if idx != -1 {
				panic(fmt.Errorf("%s: multiple prefix %q", path, prefix))
			}
			idx = i
		}
	}
	if idx == -1 {
		panic(fmt.Errorf("%s: missing prefix %q", path, prefix))
	}
	key := strings.SplitN(newLine, ":", 2)[0] + ":"
	if idx+1 < len(lines) && strings.HasPrefix(lines[idx+1], key) {
		return
	}
	out := append([]string{}, lines[:idx+1]...)
	out = append(out, newLine)
	out = append(out, lines[idx+1:]...)
	write(path, strings.Join(out, "\n")+"\n")
}

func replaceLinePrefix(path, prefix, newLine string) {
	text := read(path)
	lines := strings.Split(strings.TrimSuffix(text, "\n"), "\n")
	idx := -1
	for i, line := range lines {
		if strings.HasPrefix(line, prefix) {
			if idx != -1 {
				panic(fmt.Errorf("%s: multiple prefix %q", path, prefix))
			}
			idx = i
		}
	}
	if idx == -1 {
		panic(fmt.Errorf("%s: missing prefix %q", path, prefix))
	}
	lines[idx] = newLine
	write(path, strings.Join(lines, "\n")+"\n")
}

func copyFile(src, dst string) error {
	in, err := os.Open(src)
	if err != nil {
		return err
	}
	defer in.Close()
	if err := os.MkdirAll(filepath.Dir(dst), 0o755); err != nil {
		return err
	}
	out, err := os.Create(dst)
	if err != nil {
		return err
	}
	_, cpErr := io.Copy(out, in)
	closeErr := out.Close()
	if cpErr != nil {
		return cpErr
	}
	return closeErr
}

func walkOverlay(overlay, repo string) {
	var files []string
	must(filepath.WalkDir(overlay, func(path string, d os.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if !d.IsDir() {
			files = append(files, path)
		}
		return nil
	}))
	sort.Strings(files)
	for _, src := range files {
		rel, err := filepath.Rel(overlay, src)
		must(err)
		dst := filepath.Join(repo, rel)
		must(copyFile(src, dst))
		fmt.Println("ADD", filepath.ToSlash(rel))
	}
}

func main() {
	repo := flag.String("repo", "", "clean extracted go-discord-caller source")
	project := flag.String("project", "", "LLB Command Radio project root")
	flag.Parse()
	if *repo == "" || *project == "" {
		flag.Usage()
		os.Exit(2)
	}
	r, _ := filepath.Abs(*repo)
	p, _ := filepath.Abs(*project)
	required := []string{
		"internal/guild/raid_mode.go", "internal/bot/commands.go", "internal/bot/handlers_voice.go",
		"internal/bot/bot.go", "internal/bot/handlers_test.go", "internal/bot/setup_ui.go", "internal/guild/status.go", "internal/manager/voice_raid.go",
		"internal/manager/router/router.go", "internal/manager/pipeline/pipeline.go", "internal/manager/pipeline/star_caller.go",
	}
	for _, rel := range required {
		if _, err := os.Stat(filepath.Join(r, rel)); err != nil {
			panic(fmt.Errorf("incompatible upstream: missing %s", rel))
		}
	}
	overlay := filepath.Join(p, "server-patch", "overlay")
	if _, err := os.Stat(overlay); err != nil {
		panic(fmt.Errorf("missing overlay: %s", overlay))
	}
	walkOverlay(overlay, r)

	raid := filepath.Join(r, "internal/guild/raid_mode.go")
	replaceOnce(raid,
		"\tRaidModeOneManyGuildCaller RaidMode = \"one_many_guild_caller\"\n",
		"\tRaidModeOneManyGuildCaller RaidMode = \"one_many_guild_caller\"\n\n"+
			"\t// RaidModeCommandGuildCaller is the LLB command-radio topology. The\n"+
			"\t// owner/shotcaller broadcasts to every speaker room, while speaker-room\n"+
			"\t// capture is admitted only when that Discord user's companion helper has\n"+
			"\t// an active radio gate. Rooms remain isolated from each other.\n"+
			"\tRaidModeCommandGuildCaller RaidMode = \"command_guild_caller\"\n")
	replaceOnce(raid,
		"\treturn m == RaidModeGuildCaller || m == RaidModeAllyCaller ||\n\t\tm == RaidModeOneManyGuildCaller || m == RaidModeOneManyAllyCaller\n",
		"\treturn m == RaidModeGuildCaller || m == RaidModeAllyCaller ||\n\t\tm == RaidModeOneManyGuildCaller || m == RaidModeOneManyAllyCaller ||\n\t\tm == RaidModeCommandGuildCaller\n")
	replaceOnce(raid,
		"\treturn m == RaidModeOneManyGuildCaller || m == RaidModeOneManyAllyCaller\n",
		"\treturn m == RaidModeOneManyGuildCaller || m == RaidModeOneManyAllyCaller ||\n\t\tm == RaidModeCommandGuildCaller\n")
	replaceOnce(raid,
		"\tcase RaidModeOneManyGuildCaller:\n\t\treturn \"raid_mode.one_many_callers_host\"\n",
		"\tcase RaidModeOneManyGuildCaller:\n\t\treturn \"raid_mode.one_many_callers_host\"\n\tcase RaidModeCommandGuildCaller:\n\t\treturn \"raid_mode.command_host\"\n")
	replaceOnce(raid,
		"\t\"raid_mode.one_many_callers_host\": \"One↔Many Callers (host)\",\n",
		"\t\"raid_mode.one_many_callers_host\": \"One↔Many Callers (host)\",\n\t\"raid_mode.command_host\":          \"Command Radio (host)\",\n")

	commands := filepath.Join(r, "internal/bot/commands.go")
	replaceOnce(commands,
		"\t\t\tDescriptionLocalizations: bundle.DescriptionLocalizations(\"cmd.setup.description\"),\n\t\t},\n\t\tdiscord.SlashCommandCreate{\n\t\t\tName:                     \"start\",\n",
		"\t\t\tDescriptionLocalizations: bundle.DescriptionLocalizations(\"cmd.setup.description\"),\n\t\t\tOptions: []discord.ApplicationCommandOption{\n\t\t\t\tdiscord.ApplicationCommandOptionRole{\n\t\t\t\t\tName:                     setupCommanderRoleOption,\n\t\t\t\t\tNameLocalizations:        bundle.NameLocalizations(\"cmd.setup.opt.commander_role.name\"),\n\t\t\t\t\tDescription:              def.T(\"cmd.setup.opt.commander_role.description\"),\n\t\t\t\t\tDescriptionLocalizations: bundle.DescriptionLocalizations(\"cmd.setup.opt.commander_role.description\"),\n\t\t\t\t\tRequired:                 false,\n\t\t\t\t},\n\t\t\t\tdiscord.ApplicationCommandOptionRole{\n\t\t\t\t\tName:                     setupUnitLeaderRoleOption,\n\t\t\t\t\tNameLocalizations:        bundle.NameLocalizations(\"cmd.setup.opt.unit_leader_role.name\"),\n\t\t\t\t\tDescription:              def.T(\"cmd.setup.opt.unit_leader_role.description\"),\n\t\t\t\t\tDescriptionLocalizations: bundle.DescriptionLocalizations(\"cmd.setup.opt.unit_leader_role.description\"),\n\t\t\t\t\tRequired:                 false,\n\t\t\t\t},\n\t\t\t},\n\t\t},\n\t\tdiscord.SlashCommandCreate{\n\t\t\tName:                     \"start\",\n")
	replaceOnce(commands,
		"\t\t\t\t\t\t{Name: def.T(\"cmd.start.opt.mode.choice.one_many\"), NameLocalizations: bundle.NameLocalizations(\"cmd.start.opt.mode.choice.one_many\"), Value: callerModeOneMany},\n",
		"\t\t\t\t\t\t{Name: def.T(\"cmd.start.opt.mode.choice.one_many\"), NameLocalizations: bundle.NameLocalizations(\"cmd.start.opt.mode.choice.one_many\"), Value: callerModeOneMany},\n\t\t\t\t\t\t{Name: def.T(\"cmd.start.opt.mode.choice.command\"), NameLocalizations: bundle.NameLocalizations(\"cmd.start.opt.mode.choice.command\"), Value: callerModeCommand},\n")
	replaceOnce(commands,
		"\t\tdiscord.SlashCommandCreate{\n\t\t\tName:                     \"stop\",\n",
		"\t\tdiscord.SlashCommandCreate{\n\t\t\tName:                     \"radio-pair\",\n\t\t\tDescription:              def.T(\"cmd.radio_pair.description\"),\n\t\t\tDescriptionLocalizations: bundle.DescriptionLocalizations(\"cmd.radio_pair.description\"),\n\t\t},\n\t\tdiscord.SlashCommandCreate{\n\t\t\tName:                     \"stop\",\n")
	replaceOnce(commands, "\tcallerModeOneMany string = \"one_many\"\n", "\tcallerModeOneMany string = \"one_many\"\n\tcallerModeCommand string = \"command\"\n")
	replaceOnce(commands,
		"\tr.SlashCommand(\"/start\", h.withManager(h.handleStartVoiceRaid))\n",
		"\tr.SlashCommand(\"/start\", h.withManager(h.handleStartVoiceRaid))\n\tr.SlashCommand(\"/radio-pair\", h.withGuild(h.handleRadioPair))\n")

	hv := filepath.Join(r, "internal/bot/handlers_voice.go")
	replaceOnce(hv,
		"func (h *CommandHandlers) handleSetup(guildID snowflake.ID, loc *i18n.Localizer, _ discord.SlashCommandInteractionData, e *handler.CommandEvent) error {\n",
		"func (h *CommandHandlers) handleSetup(guildID snowflake.ID, loc *i18n.Localizer, data discord.SlashCommandInteractionData, e *handler.CommandEvent) error {\n")
	replaceOnce(hv,
		"\tif h.manager.HasActiveSession(guildID) {\n\t\treturn e.CreateMessage(ephemeral(loc.T(\"setup.blocked_active_raid\")))\n\t}\n\n\tmsg, components := h.buildMainSetupMessage(guildID, loc)\n",
		"\tif h.manager.HasActiveSession(guildID) {\n\t\treturn e.CreateMessage(ephemeral(loc.T(\"setup.blocked_active_raid\")))\n\t}\n\n\tvar commanderRole, unitLeaderRole *snowflake.ID\n\tif role, ok := data.OptRole(setupCommanderRoleOption); ok {\n\t\troleID := role.ID\n\t\tcommanderRole = &roleID\n\t}\n\tif role, ok := data.OptRole(setupUnitLeaderRoleOption); ok {\n\t\troleID := role.ID\n\t\tunitLeaderRole = &roleID\n\t}\n\tbindCommandRadioSetupRoles(h.manager, guildID, commanderRole, unitLeaderRole)\n\n\tmsg, components := h.buildMainSetupMessage(guildID, loc)\n")
	replaceOnce(hv,
		"\tif hasCode && code != \"\" {\n\t\tvar mode guild.RaidMode\n\t\tswitch modeStr {\n\t\tcase callerModeMany:\n\t\t\tmode = guild.RaidModeAllyCaller\n\t\tcase callerModeOneMany:\n\t\t\tmode = guild.RaidModeOneManyAllyCaller\n\t\tdefault:\n\t\t\tmode = guild.RaidModeAllyListener\n\t\t}\n",
		"\tif hasCode && code != \"\" {\n\t\tmode := resolveStartMode(true, modeStr)\n")
	replaceOnce(hv,
		"\tvar mode guild.RaidMode\n\tswitch modeStr {\n\tcase callerModeMany:\n\t\tmode = guild.RaidModeGuildCaller\n\tcase callerModeOneMany:\n\t\tmode = guild.RaidModeOneManyGuildCaller\n\tdefault:\n\t\tmode = guild.RaidModeOneCaller\n\t}\n",
		"\tmode := resolveStartMode(false, modeStr)\n")
	replaceOnce(hv,
		"\t\"github.com/sealbro/go-discord-caller/internal/guild\"\n",
		"")

	botgo := filepath.Join(r, "internal/bot/bot.go")
	replaceOnce(botgo,
		"type LifecycleManager interface {\n\tStartMetrics()\n\tShutdown(ctx context.Context)\n}\n",
		"type LifecycleManager interface {\n\tStartMetrics()\n\tStartRadioAPI(ctx context.Context) error\n\tShutdown(ctx context.Context)\n}\n")
	replaceOnce(botgo,
		"\tb.manager.StartMetrics()\n\n\tif err := b.client.OpenGateway(ctx); err != nil {\n",
		"\tb.manager.StartMetrics()\n\tgo func() {\n\t\tif err := b.manager.StartRadioAPI(ctx); err != nil {\n\t\t\tslog.ErrorContext(ctx, \"command radio API stopped\", slog.Any(\"err\", err))\n\t\t}\n\t}()\n\n\tif err := b.client.OpenGateway(ctx); err != nil {\n")
	replaceOnce(botgo,
		"type ManagerService interface {\n\tSessionManager\n",
		"type RadioManager interface {\n\tIssueRadioPairCode(guildID, userID snowflake.ID) (string, error)\n}\n\ntype ManagerService interface {\n\tSessionManager\n")
	replaceOnce(botgo,
		"\tLifecycleManager\n}\n",
		"\tLifecycleManager\n\tRadioManager\n}\n")

	botTests := filepath.Join(r, "internal/bot/handlers_test.go")
	replaceOnce(botTests,
		"func (f *fakeManager) StartMetrics()                                        {}\nfunc (f *fakeManager) Shutdown(context.Context)                             {}\n",
		"func (f *fakeManager) StartMetrics()                                        {}\nfunc (f *fakeManager) StartRadioAPI(context.Context) error                  { return nil }\nfunc (f *fakeManager) IssueRadioPairCode(snowflake.ID, snowflake.ID) (string, error) { return \"\", nil }\nfunc (f *fakeManager) Shutdown(context.Context)                             {}\n")

	voice := filepath.Join(r, "internal/manager/voice_raid.go")
	replaceOnce(voice,
		"\tallowUser := m.buildAllowUserFilter(guildID)\n\tsetup, err := m.setupSpeakers(ctx, guildID, mode, allowUser.Check)\n",
		"\tallowUser := m.buildAllowUserFilter(guildID)\n\tspeakerAllow := allowUser.Check\n\townerAllow := allowUser.Check\n\townerRouteRoleID := allowUser.RoleID()\n\tif mode == guild.RaidModeCommandGuildCaller {\n\t\tspeakerAllow = m.commandRadioAllow(guildID, allowUser.Check)\n\t\townerAllow = m.commandRadioCommanderAllow(guildID)\n\t\townerRouteRoleID = m.commandRadioCommanderRoleID(guildID)\n\t}\n\tsetup, err := m.setupSpeakers(ctx, guildID, mode, speakerAllow)\n")
	replaceOnce(voice,
		"\tgm := m.metrics.ForGuild(ctx, guildID)\n\townerSetup := NewVoiceConnSetup(m.ownerBotID).WithVoiceReceiver(allowUser.Check, gm.Receiver())\n",
		"\tgm := m.metrics.ForGuild(ctx, guildID)\n\townerSetup := NewVoiceConnSetup(m.ownerBotID).WithVoiceReceiver(ownerAllow, gm.Receiver())\n")
	replaceOnce(voice,
		"\tm.storeApplier(guildID, m.ownerBotID, m.buildApplier(guildID, m.ownerBotID, chOwnerOut, ownerHandle, allowUser.Check))\n",
		"\tm.storeApplier(guildID, m.ownerBotID, m.buildApplier(guildID, m.ownerBotID, chOwnerOut, ownerHandle, ownerAllow))\n")
	replaceOnce(voice,
		"\tGM:           gm,\n\t\tAllowFilter:  allowUser,\n",
		"\tGM:                gm,\n\t\tAllowFilter:       allowUser,\n\t\tOwnerRouteRoleID: ownerRouteRoleID,\n")

	routerGo := filepath.Join(r, "internal/manager/router/router.go")
	replaceOnce(routerGo,
		"// router): ID, ChannelID, Handle, Feeds, BuildInstall. The router only\n",
		"// router): ID, ChannelID, RoleID, Handle, Feeds, BuildInstall. The router only\n")
	replaceOnce(routerGo,
		"\tID           snowflake.ID\n\tChannelID    snowflake.ID\n\tHandle       *opus.FanoutHandle\n",
		"\tID           snowflake.ID\n\tChannelID    snowflake.ID\n\t// RoleID overrides the router's session-wide role for this source. Zero\n\t// preserves the legacy session-wide role behaviour.\n\tRoleID       snowflake.ID\n\tHandle       *opus.FanoutHandle\n")
	replaceOnce(routerGo,
		"type routeSource struct {\n\tid        snowflake.ID\n\tchannelID snowflake.ID\n}\n",
		"type routeSource struct {\n\tid        snowflake.ID\n\tchannelID snowflake.ID\n\troleID    snowflake.ID\n}\n")
	replaceOnce(routerGo,
		"\tfor _, s := range sources {\n\t\tswitch c := callerCounts[s.channelID]; {\n",
		"\tfor _, s := range sources {\n\t\tswitch c := callerCounts[s.id]; {\n")
	replaceOnce(routerGo,
		"\trouteSources := make([]routeSource, 0, len(r.sources))\n\tuniqueChannels := make([]snowflake.ID, 0, len(r.sources))\n\tseenChannels := make(map[snowflake.ID]struct{}, len(r.sources))\n\tfor _, s := range r.sources {\n\t\trouteSources = append(routeSources, routeSource{id: s.ID, channelID: s.ChannelID})\n\t\tif _, ok := seenChannels[s.ChannelID]; !ok {\n\t\t\tseenChannels[s.ChannelID] = struct{}{}\n\t\t\tuniqueChannels = append(uniqueChannels, s.ChannelID)\n\t\t}\n\t}\n",
		"\trouteSources := make([]routeSource, 0, len(r.sources))\n\tfor _, s := range r.sources {\n\t\trouteSources = append(routeSources, routeSource{id: s.ID, channelID: s.ChannelID, roleID: s.RoleID})\n\t}\n")
	replaceOnce(routerGo,
		"\troleID := r.roleID\n\tr.mu.Unlock()\n",
		"\tsessionRoleID := r.roleID\n\tr.mu.Unlock()\n")
	replaceOnce(routerGo,
		"\tusersPerChannel := make(map[snowflake.ID][]snowflake.ID, len(uniqueChannels))\n\tcallerCounts := make(map[snowflake.ID]int, len(uniqueChannels))\n\tfor _, chID := range uniqueChannels {\n\t\tusers := r.enumerator.EnumerateCallers(chID, roleID)\n\t\tusersPerChannel[chID] = users\n\t\tcallerCounts[chID] = len(users)\n\t}\n",
		"\tusersPerSource := make(map[snowflake.ID][]snowflake.ID, len(routeSources))\n\tcallerCounts := make(map[snowflake.ID]int, len(routeSources))\n\tfor _, source := range routeSources {\n\t\troleID := source.roleID\n\t\tif roleID == 0 {\n\t\t\troleID = sessionRoleID\n\t\t}\n\t\tusers := r.enumerator.EnumerateCallers(source.channelID, roleID)\n\t\tusersPerSource[source.id] = users\n\t\tcallerCounts[source.id] = len(users)\n\t}\n")
	replaceOnce(routerGo,
		"\tr.applyModes(sourceModes, destMix, usersPerChannel, listenersPerChannel)\n",
		"\tr.applyModes(sourceModes, destMix, usersPerSource, listenersPerChannel)\n")
	replaceOnce(routerGo,
		"func (r *Router) applyModes(sourceModes map[snowflake.ID]RouteMode, destMix map[snowflake.ID]bool, usersPerChannel map[snowflake.ID][]snowflake.ID, listenersPerChannel map[snowflake.ID]bool) {\n",
		"func (r *Router) applyModes(sourceModes map[snowflake.ID]RouteMode, destMix map[snowflake.ID]bool, usersPerSource map[snowflake.ID][]snowflake.ID, listenersPerChannel map[snowflake.ID]bool) {\n")
	replaceOnce(routerGo,
		"\t\tusers := usersPerChannel[s.ChannelID]\n",
		"\t\tusers := usersPerSource[id]\n")

	pipelineGo := filepath.Join(r, "internal/manager/pipeline/pipeline.go")
	replaceOnce(pipelineGo,
		"\tAllowFilter  AllowSource\n\tVoiceProbe   router.VoiceProbe // production: *manager.cacheVoiceProbe\n",
		"\tAllowFilter       AllowSource\n\t// OwnerRouteRoleID overrides the owner source's routing role. It is set\n\t// only for Command Radio; zero preserves legacy caller-role routing.\n\tOwnerRouteRoleID snowflake.ID\n\tVoiceProbe        router.VoiceProbe // production: *manager.cacheVoiceProbe\n")

	star := filepath.Join(r, "internal/manager/pipeline/star_caller.go")
	replaceOnce(star,
		"\t\tslot := &router.SourceSlot{\n\t\t\tID:        e.ID,\n\t\t\tChannelID: e.ChannelID,\n\t\t\tHandle:    e.Handle,\n\t\t}\n\t\tif e.ChannelID == p.OV.ChannelID() {\n",
		"\t\tslot := &router.SourceSlot{\n\t\t\tID:        e.ID,\n\t\t\tChannelID: e.ChannelID,\n\t\t\tHandle:    e.Handle,\n\t\t}\n\t\tif e.ChannelID == p.OV.ChannelID() {\n\t\t\tslot.RoleID = p.OwnerRouteRoleID\n")

	setupUI := filepath.Join(r, "internal/bot/setup_ui.go")
	replaceOnce(setupUI,
		"loc.T(\"setup.select_caller_role\")",
		"loc.T(\"radio.setup.select_unit_leader_role\")")
	replaceOnce(setupUI,
		"loc.T(\"setup.select_manager_role\")",
		"loc.T(\"radio.setup.select_commander_role\")")
	replaceOnce(setupUI,
		"return loc.T(\"setup.roles_title\") + \"\\n\" + status.Render(loc), components\n",
		"return loc.T(\"radio.setup.roles_title\") + \"\\n\" + loc.T(\"radio.setup.roles_hint\") + \"\\n\" + status.Render(loc), components\n")

	status := filepath.Join(r, "internal/guild/status.go")
	replaceOnce(status,
		"\tif s.CallerRoleID != nil {\n\t\tfmt.Fprintf(&sb, \"\\n**%s:** <@&%s>\\n\", t(\"status.capture_role\"), s.CallerRoleID)\n\t} else {\n\t\tfmt.Fprintf(&sb, \"\\n**%s:** %s\\n\", t(\"status.capture_role\"), t(\"status.not_set\"))\n\t}\n",
		"\tif s.CallerRoleID != nil {\n\t\tfmt.Fprintf(&sb, \"\\n**%s:** <@&%s>\\n\", t(\"radio.status.unit_leader_role\"), s.CallerRoleID)\n\t} else {\n\t\tfmt.Fprintf(&sb, \"\\n**%s:** %s\\n\", t(\"radio.status.unit_leader_role\"), t(\"status.not_set\"))\n\t}\n")
	replaceOnce(status,
		"\tif s.ManagerRoleID != nil {\n\t\tfmt.Fprintf(&sb, \"\\n**%s:** <@&%s>\\n\", t(\"status.manager_role\"), s.ManagerRoleID)\n\t} else {\n\t\tfmt.Fprintf(&sb, \"\\n**%s:** %s\\n\", t(\"status.manager_role\"), t(\"status.not_set\"))\n\t}\n",
		"\tif s.ManagerRoleID != nil {\n\t\tfmt.Fprintf(&sb, \"\\n**%s:** <@&%s>\\n\", t(\"radio.status.commander_role\"), s.ManagerRoleID)\n\t} else {\n\t\tfmt.Fprintf(&sb, \"\\n**%s:** %s\\n\", t(\"radio.status.commander_role\"), t(\"status.not_set\"))\n\t}\n")
	replaceOnce(status,
		"\tif chID, ok := s.BoundChannels[s.OwnerUserID]; ok {\n\t\tfmt.Fprintf(&sb, \"\\n**%s:** <#%s>\\n\", t(\"status.owner_channel\"), chID)\n\t} else {\n\t\tfmt.Fprintf(&sb, \"\\n**%s:** %s\\n\", t(\"status.owner_channel\"), t(\"status.not_set\"))\n\t}\n",
		"\tif chID, ok := s.BoundChannels[s.OwnerUserID]; ok {\n\t\tfmt.Fprintf(&sb, \"\\n**%s:** <#%s>\\n\", t(\"radio.status.command_channel\"), chID)\n\t} else {\n\t\tfmt.Fprintf(&sb, \"\\n**%s:** %s\\n\", t(\"radio.status.command_channel\"), t(\"status.not_set\"))\n\t}\n")
	replaceOnce(status,
		"\tcase \"status.capture_role\":\n\t\treturn \"Capture Role\"\n",
		"\tcase \"radio.status.unit_leader_role\":\n\t\treturn \"Unit Leader Role\"\n")
	replaceOnce(status,
		"\tcase \"status.manager_role\":\n\t\treturn \"Manager Role\"\n",
		"\tcase \"radio.status.commander_role\":\n\t\treturn \"Commander Role\"\n")
	replaceOnce(status,
		"\tcase \"status.owner_channel\":\n\t\treturn \"Owner Bot Channel\"\n",
		"\tcase \"radio.status.command_channel\":\n\t\treturn \"Command Channel\"\n")

	locales, err := filepath.Glob(filepath.Join(r, "internal/i18n/locales/*.yaml"))
	must(err)
	sort.Strings(locales)
	startModeDescriptions := map[string]string{
		"de.yaml": `cmd.start.opt.mode.description: "Audiomodus: lokal ist Command Radio Standard; mit Relay-Code bleibt Zuhören Standard."`,
		"en.yaml": `cmd.start.opt.mode.description: "Audio mode: local default is Command Radio; relay-code joins default to listener."`,
		"es.yaml": `cmd.start.opt.mode.description: "Audio: Command Radio es el predeterminado local; con código relay, oyente es predeterminado."`,
		"fr.yaml": `cmd.start.opt.mode.description: "Audio : Command Radio est le défaut local ; avec code relay, auditeur est le défaut."`,
		"pl.yaml": `cmd.start.opt.mode.description: "Audio: Command Radio jest lokalnym domyślnym; z kodem relay domyślny jest nasłuch."`,
		"pt.yaml": `cmd.start.opt.mode.description: "Áudio: Command Radio é o padrão local; com código relay, ouvinte é o padrão."`,
		"ru.yaml": `cmd.start.opt.mode.description: "Аудио: локально по умолчанию Command Radio; с relay-кодом — режим слушателя."`,
	}
	for _, loc := range locales {
		replacement, ok := startModeDescriptions[filepath.Base(loc)]
		if !ok {
			panic(fmt.Errorf("missing /start mode copy for locale %s", loc))
		}
		replaceLinePrefix(loc, "cmd.start.opt.mode.description:", replacement)
		appendAfterLinePrefix(loc, "cmd.setup.description:", `cmd.setup.opt.commander_role.name: "commander-role"`)
		appendAfterLinePrefix(loc, "cmd.setup.opt.commander_role.name:", `cmd.setup.opt.commander_role.description: "Commander Role: controls Command Radio and broadcasts from the Command Channel."`)
		appendAfterLinePrefix(loc, "cmd.setup.opt.commander_role.description:", `cmd.setup.opt.unit_leader_role.name: "unit-leader-role"`)
		appendAfterLinePrefix(loc, "cmd.setup.opt.unit_leader_role.name:", `cmd.setup.opt.unit_leader_role.description: "Unit Leader Role: may uplink from Unit channels while Command Radio is active."`)
		appendAfterLinePrefix(loc, "setup.select_caller_role:", `radio.setup.select_unit_leader_role: "Unit Leader Role — gated uplink"`)
		appendAfterLinePrefix(loc, "setup.select_manager_role:", `radio.setup.select_commander_role: "Commander Role — control and broadcast"`)
		appendAfterLinePrefix(loc, "setup.roles_title:", `radio.setup.roles_title: "📻 Command Radio Roles"`)
		appendAfterLinePrefix(loc, "radio.setup.roles_title:", `radio.setup.roles_hint: "Can't find a role? Run /setup with commander-role or unit-leader-role."`)
		appendAfterLinePrefix(loc, "status.capture_role:", `radio.status.unit_leader_role: "Unit Leader Role"`)
		appendAfterLinePrefix(loc, "status.manager_role:", `radio.status.commander_role: "Commander Role"`)
		appendAfterLinePrefix(loc, "status.owner_channel:", `radio.status.command_channel: "Command Channel"`)
		appendAfterLinePrefix(loc, "cmd.start.opt.mode.choice.one_many:", `cmd.start.opt.mode.choice.command: "Command Radio"`)
		appendAfterLinePrefix(loc, "cmd.status.description:", `cmd.radio_pair.description: "Pair this PC with LLB Command Radio"`)
		appendAfterLinePrefix(loc, "raid.start_failed:", `radio.need_caller: "❌ You need the configured capture/caller role to pair Command Radio."`)
		appendAfterLinePrefix(loc, "radio.need_caller:", `radio.pair_failed: "❌ Failed to create radio pairing code: {{.Err}}"`)
		appendAfterLinePrefix(loc, "radio.pair_failed:", `radio.pair_code: "📻 Command Radio pair code: {{.Code}} — expires in 5 minutes and works once."`)
		appendAfterLinePrefix(loc, "raid_mode.one_many_callers_host:", `raid_mode.command_host: "Command Radio (host)"`)
	}

	fmt.Println("\nPATCH APPLIED")
	fmt.Println("Upstream compatibility target:", baseHint)
}
