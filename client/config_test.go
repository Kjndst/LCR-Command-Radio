package main

import "testing"

func TestRadioKeyDefaultAndPersistence(t *testing.T) {
	if got := defaultConfig().RadioKey; got != "Mouse5" {
		t.Fatalf("default radio key = %q, want Mouse5", got)
	}
	t.Setenv("XDG_CONFIG_HOME", t.TempDir())
	want := defaultConfig()
	want.RadioKey = "F8"
	want.RadioKeyKind = keyKindKeyboard
	want.RadioKeyCode = 0x77
	if err := saveConfig(want); err != nil {
		t.Fatal(err)
	}
	got := loadConfig()
	if got.RadioKey != want.RadioKey || got.RadioKeyKind != want.RadioKeyKind || got.RadioKeyCode != want.RadioKeyCode {
		t.Fatalf("saved radio key = %#v, want %#v", got, want)
	}
}
