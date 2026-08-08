package main

import "testing"

func TestMouseKeyNames(t *testing.T) {
	if got := mouseKeyName(2); got != "Mouse5" {
		t.Fatalf("Mouse5 = %q", got)
	}
	if got := mouseKeyName(1); got != "Mouse4" {
		t.Fatalf("Mouse4 = %q", got)
	}
}

func TestFriendlyVirtualKeys(t *testing.T) {
	for vk, want := range map[uint32]string{0x77: "F8", 0xA4: "Left Alt", 0x14: "Caps Lock"} {
		if got := friendlyVirtualKey(vk); got != want {
			t.Errorf("friendlyVirtualKey(%#x) = %q, want %q", vk, got, want)
		}
	}
}
