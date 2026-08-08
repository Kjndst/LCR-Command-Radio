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
    if err != nil { panic(err) }
}

func read(path string) string {
    b, err := os.ReadFile(path); must(err); return string(b)
}

func write(path, s string) {
    must(os.WriteFile(path, []byte(s), 0o644))
}

func replaceOnce(path, old, neu string) {
    text := read(path)
    n := strings.Count(text, old)
    if n != 1 { panic(fmt.Errorf("%s: expected anchor exactly once, found %d: %q", path, n, trim(old, 100))) }
    write(path, strings.Replace(text, old, neu, 1))
}

func trim(s string, n int) string { if len(s) > n { return s[:n] }; return s }

func appendAfterLinePrefix(path, prefix, newLine string) {
    text := read(path)
    lines := strings.Split(strings.TrimSuffix(text, "\n"), "\n")
    idx := -1
    for i, line := range lines { if strings.HasPrefix(line, prefix) { if idx != -1 { panic(fmt.Errorf("%s: multiple prefix %q", path, prefix)) }; idx = i } }
    if idx == -1 { panic(fmt.Errorf("%s: missing prefix %q", path, prefix)) }
    key := strings.SplitN(newLine, ":", 2)[0] + ":"
    if idx+1 < len(lines) && strings.HasPrefix(lines[idx+1], key) { return }
    out := append([]string{}, lines[:idx+1]...)
    out = append(out, newLine)
    out = append(out, lines[idx+1:]...)
    write(path, strings.Join(out, "\n")+"\n")
}

func copyFile(src, dst string) error {
    in, err := os.Open(src); if err != nil { return err }; defer in.Close()
    if err := os.MkdirAll(filepath.Dir(dst), 0o755); err != nil { return err }
    out, err := os.Create(dst); if err != nil { return err }
    _, cpErr := io.Copy(out, in); closeErr := out.Close()
    if cpErr != nil { return cpErr }; return closeErr
}

func walkOverlay(overlay, repo string) {
    var files []string
    must(filepath.WalkDir(overlay, func(path string, d os.DirEntry, err error) error {
        if err != nil { return err }
        if !d.IsDir() { files = append(files, path) }
        return nil
    }))
    sort.Strings(files)
    for _, src := range files {
        rel, err := filepath.Rel(overlay, src); must(err)
        dst := filepath.Join(repo, rel)
        must(copyFile(src, dst))
        fmt.Println("ADD", filepath.ToSlash(rel))
    }
}

func main() {
    repo := flag.String("repo", "", "clean extracted go-discord-caller source")
    project := flag.String("project", "", "LLB Command Radio project root")
    flag.Parse()
    if *repo == "" || *project == "" { flag.Usage(); os.Exit(2) }
    r, _ := filepath.Abs(*repo); p, _ := filepath.Abs(*project)
    required := []string{
        "internal/guild/raid_mode.go", "internal/bot/commands.go", "internal/bot/handlers_voice.go",
        "internal/bot/bot.go", "internal/manager/voice_raid.go",
    }
    for _, rel := range required { if _, err := os.Stat(filepath.Join(r, rel)); err != nil { panic(fmt.Errorf("incompatible upstream: missing %s", rel)) } }
    overlay := filepath.Join(p, "server-patch", "overlay")
    if _, err := os.Stat(overlay); err != nil { panic(fmt.Errorf("missing overlay: %s", overlay)) }
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
        "\tcase callerModeOneMany:\n\t\tmode = guild.RaidModeOneManyGuildCaller\n\tdefault:\n",
        "\tcase callerModeOneMany:\n\t\tmode = guild.RaidModeOneManyGuildCaller\n\tcase callerModeCommand:\n\t\tmode = guild.RaidModeCommandGuildCaller\n\tdefault:\n")

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

    voice := filepath.Join(r, "internal/manager/voice_raid.go")
    replaceOnce(voice,
        "\tallowUser := m.buildAllowUserFilter(guildID)\n\tsetup, err := m.setupSpeakers(ctx, guildID, mode, allowUser.Check)\n",
        "\tallowUser := m.buildAllowUserFilter(guildID)\n\tspeakerAllow := allowUser.Check\n\tif mode == guild.RaidModeCommandGuildCaller {\n\t\tspeakerAllow = m.commandRadioAllow(guildID, allowUser.Check)\n\t}\n\tsetup, err := m.setupSpeakers(ctx, guildID, mode, speakerAllow)\n")

    locales, err := filepath.Glob(filepath.Join(r, "internal/i18n/locales/*.yaml")); must(err); sort.Strings(locales)
    for _, loc := range locales {
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
