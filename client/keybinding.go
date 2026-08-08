package main

import "fmt"

const (
	keyKindMouse    = "mouse"
	keyKindKeyboard = "keyboard"
)

func mouseKeyName(button uint16) string {
	switch button {
	case 1:
		return "Mouse4"
	case 2:
		return "Mouse5"
	case 3:
		return "Left Mouse"
	case 4:
		return "Right Mouse"
	case 5:
		return "Middle Mouse"
	default:
		return ""
	}
}

func friendlyVirtualKey(vk uint32) string {
	switch {
	case vk >= 0x70 && vk <= 0x87:
		return fmt.Sprintf("F%d", vk-0x70+1)
	case vk >= 'A' && vk <= 'Z':
		return string(rune(vk))
	case vk >= '0' && vk <= '9':
		return string(rune(vk))
	}
	switch vk {
	case 0x14:
		return "Caps Lock"
	case 0x20:
		return "Space"
	case 0x09:
		return "Tab"
	case 0x0D:
		return "Enter"
	case 0x08:
		return "Backspace"
	case 0x25:
		return "Left Arrow"
	case 0x26:
		return "Up Arrow"
	case 0x27:
		return "Right Arrow"
	case 0x28:
		return "Down Arrow"
	case 0xA4:
		return "Left Alt"
	case 0xA5:
		return "Right Alt"
	case 0xA2:
		return "Left Ctrl"
	case 0xA3:
		return "Right Ctrl"
	case 0xA0:
		return "Left Shift"
	case 0xA1:
		return "Right Shift"
	}
	return ""
}
