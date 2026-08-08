//go:build windows

package main

import (
	"context"
	"embed"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"sync"
	"syscall"
	"time"
	"unsafe"
)

//go:embed assets/*.ico
var iconFS embed.FS

const (
	appName   = "LLB Command Radio"
	className = "LLBCommandRadioWindow"

	WM_DESTROY        = 0x0002
	WM_CLOSE          = 0x0010
	WM_PAINT          = 0x000F
	WM_LBUTTONUP      = 0x0202
	WM_RBUTTONUP      = 0x0205
	WM_LBUTTONDBLCLK  = 0x0203
	WM_XBUTTONDOWN    = 0x020B
	WM_XBUTTONUP      = 0x020C
	WM_CTLCOLORSTATIC = 0x0138
	WM_CTLCOLOREDIT   = 0x0133
	WM_COMMAND        = 0x0111
	WM_APP            = 0x8000
	WM_TRAY           = WM_APP + 1
	WM_STATE          = WM_APP + 2

	WH_MOUSE_LL = 14
	HC_ACTION   = 0
	XBUTTON1    = 1
	XBUTTON2    = 2

	WS_OVERLAPPED  = 0x00000000
	WS_CAPTION     = 0x00C00000
	WS_SYSMENU     = 0x00080000
	WS_MINIMIZEBOX = 0x00020000
	WS_VISIBLE     = 0x10000000
	WS_CHILD       = 0x40000000
	WS_BORDER      = 0x00800000
	SS_CENTER      = 0x00000001
	SS_NOTIFY      = 0x00000100
	SS_CENTERIMAGE = 0x00000200
	ES_CENTER      = 0x0001
	ES_UPPERCASE   = 0x0008

	SW_HIDE       = 0
	SW_SHOW       = 5
	SW_SHOWNORMAL = 1

	CW_USEDEFAULT = 0x80000000
	COLOR_WINDOW  = 5
	IDC_ARROW     = 32512

	NIM_ADD     = 0x00000000
	NIM_MODIFY  = 0x00000001
	NIM_DELETE  = 0x00000002
	NIF_MESSAGE = 0x00000001
	NIF_ICON    = 0x00000002
	NIF_TIP     = 0x00000004

	IMAGE_ICON      = 1
	LR_LOADFROMFILE = 0x00000010
	LR_DEFAULTSIZE  = 0x00000040

	TPM_RIGHTBUTTON = 0x0002
	TPM_RETURNCMD   = 0x0100

	MF_STRING    = 0x0000
	MF_SEPARATOR = 0x0800
	MF_CHECKED   = 0x0008

	TRANSPARENT   = 1
	DT_LEFT       = 0x00000000
	DT_CENTER     = 0x00000001
	DT_VCENTER    = 0x00000004
	DT_SINGLELINE = 0x00000020

	MB_YESNO        = 0x00000004
	MB_ICONQUESTION = 0x00000020
	IDYES           = 6

	cmdOpen        = 1001
	cmdKeyMouse4   = 1002
	cmdKeyMouse5   = 1003
	cmdModeHold    = 1004
	cmdModeToggle  = 1005
	cmdVoiceOpen   = 1006
	cmdVoicePTT    = 1007
	cmdRepair      = 1008
	cmdUninstall   = 1009
	cmdExit        = 1010
	ctrlPairFinish = 2001
)

type POINT struct{ X, Y int32 }
type RECT struct{ Left, Top, Right, Bottom int32 }
type MSG struct {
	HWnd     uintptr
	Message  uint32
	WParam   uintptr
	LParam   uintptr
	Time     uint32
	Pt       POINT
	LPrivate uint32
}
type WNDCLASSEX struct {
	CbSize        uint32
	Style         uint32
	LpfnWndProc   uintptr
	CbClsExtra    int32
	CbWndExtra    int32
	HInstance     uintptr
	HIcon         uintptr
	HCursor       uintptr
	HbrBackground uintptr
	LpszMenuName  *uint16
	LpszClassName *uint16
	HIconSm       uintptr
}
type PAINTSTRUCT struct {
	Hdc         uintptr
	FErase      int32
	RcPaint     RECT
	FRestore    int32
	FIncUpdate  int32
	RgbReserved [32]byte
}
type NOTIFYICONDATA struct {
	CbSize            uint32
	HWnd              uintptr
	UID               uint32
	UFlags            uint32
	UCallbackMessage  uint32
	HIcon             uintptr
	SzTip             [128]uint16
	DwState           uint32
	DwStateMask       uint32
	SzInfo            [256]uint16
	UTimeoutOrVersion uint32
	SzInfoTitle       [64]uint16
	DwInfoFlags       uint32
	GuidItem          [16]byte
	HBalloonIcon      uintptr
}
type MSLLHOOKSTRUCT struct {
	Pt          POINT
	MouseData   uint32
	Flags       uint32
	Time        uint32
	DwExtraInfo uintptr
}

var (
	user32   = syscall.NewLazyDLL("user32.dll")
	kernel32 = syscall.NewLazyDLL("kernel32.dll")
	gdi32    = syscall.NewLazyDLL("gdi32.dll")
	shell32  = syscall.NewLazyDLL("shell32.dll")
	dwmapi   = syscall.NewLazyDLL("dwmapi.dll")

	pRegisterClassEx     = user32.NewProc("RegisterClassExW")
	pCreateWindowEx      = user32.NewProc("CreateWindowExW")
	pDefWindowProc       = user32.NewProc("DefWindowProcW")
	pShowWindow          = user32.NewProc("ShowWindow")
	pUpdateWindow        = user32.NewProc("UpdateWindow")
	pGetMessage          = user32.NewProc("GetMessageW")
	pTranslateMessage    = user32.NewProc("TranslateMessage")
	pDispatchMessage     = user32.NewProc("DispatchMessageW")
	pPostQuitMessage     = user32.NewProc("PostQuitMessage")
	pPostMessage         = user32.NewProc("PostMessageW")
	pBeginPaint          = user32.NewProc("BeginPaint")
	pEndPaint            = user32.NewProc("EndPaint")
	pLoadCursor          = user32.NewProc("LoadCursorW")
	pInvalidateRect      = user32.NewProc("InvalidateRect")
	pSetWindowText       = user32.NewProc("SetWindowTextW")
	pSendMessage         = user32.NewProc("SendMessageW")
	pGetWindowText       = user32.NewProc("GetWindowTextW")
	pSetWindowPos        = user32.NewProc("SetWindowPos")
	pMessageBox          = user32.NewProc("MessageBoxW")
	pSetForegroundWindow = user32.NewProc("SetForegroundWindow")
	pGetCursorPos        = user32.NewProc("GetCursorPos")
	pCreatePopupMenu     = user32.NewProc("CreatePopupMenu")
	pAppendMenu          = user32.NewProc("AppendMenuW")
	pTrackPopupMenu      = user32.NewProc("TrackPopupMenu")
	pDestroyMenu         = user32.NewProc("DestroyMenu")
	pLoadImage           = user32.NewProc("LoadImageW")
	pDestroyIcon         = user32.NewProc("DestroyIcon")
	pSetWindowsHookEx    = user32.NewProc("SetWindowsHookExW")
	pUnhookWindowsHookEx = user32.NewProc("UnhookWindowsHookEx")
	pCallNextHookEx      = user32.NewProc("CallNextHookEx")
	pGetClientRect       = user32.NewProc("GetClientRect")

	pGetModuleHandle = kernel32.NewProc("GetModuleHandleW")

	pCreateSolidBrush = gdi32.NewProc("CreateSolidBrush")
	pDeleteObject     = gdi32.NewProc("DeleteObject")
	pFillRect         = user32.NewProc("FillRect")
	pSetTextColor     = gdi32.NewProc("SetTextColor")
	pSetBkColor       = gdi32.NewProc("SetBkColor")
	pSetBkMode        = gdi32.NewProc("SetBkMode")
	pCreateFont       = gdi32.NewProc("CreateFontW")
	pSelectObject     = gdi32.NewProc("SelectObject")
	pDrawText         = user32.NewProc("DrawTextW")

	pShellNotifyIcon       = shell32.NewProc("Shell_NotifyIconW")
	pDwmSetWindowAttribute = dwmapi.NewProc("DwmSetWindowAttribute")
)

func rgb(r, g, b byte) uintptr      { return uintptr(r) | uintptr(g)<<8 | uintptr(b)<<16 }
func ptr(s string) *uint16          { p, _ := syscall.UTF16PtrFromString(s); return p }
func setTip(dst []uint16, s string) { u, _ := syscall.UTF16FromString(s); copy(dst, u) }

var currentApp *winApp

type winApp struct {
	mu                             sync.RWMutex
	cfg                            AppConfig
	api                            *RadioAPI
	hwnd                           uintptr
	edit                           uintptr
	pairBtn                        uintptr
	hook                           uintptr
	paired                         bool
	pairing                        bool
	pairError                      string
	connected                      bool
	transmitting                   bool
	quitting                       bool
	heartbeatCancel                context.CancelFunc
	radioEvents                    chan bool
	iconGreen, iconRed, iconGray   uintptr
	brushBG, brushPanel, brushEdit uintptr
	fontTitle, fontBody, fontSmall uintptr
}

func run(args []string) error {
	runtime.LockOSThread()
	if hasArg(args, "--uninstall") {
		return performUninstall()
	}
	portable := hasArg(args, "--portable")
	if !portable {
		installed, err := ensureInstalled()
		if err != nil {
			return err
		}
		if !installed {
			return nil
		}
	}
	a := &winApp{cfg: loadConfig(), radioEvents: make(chan bool, 16)}
	a.api = NewRadioAPI(a.cfg.ServerURL, a.cfg.DeviceToken)
	a.paired = a.cfg.DeviceToken != ""
	currentApp = a
	if err := a.init(); err != nil {
		return err
	}
	defer a.cleanup()
	go a.radioWorker()
	go a.probeLoop()
	return a.messageLoop()
}

func hasArg(args []string, s string) bool {
	for _, a := range args {
		if strings.EqualFold(a, s) {
			return true
		}
	}
	return false
}

func (a *winApp) init() error {
	hinst, _, _ := pGetModuleHandle.Call(0)
	cursor, _, _ := pLoadCursor.Call(0, IDC_ARROW)
	class := ptr(className)
	wc := WNDCLASSEX{CbSize: uint32(unsafe.Sizeof(WNDCLASSEX{})), LpfnWndProc: syscall.NewCallback(wndProc), HInstance: hinst, HCursor: cursor, LpszClassName: class}
	if r, _, e := pRegisterClassEx.Call(uintptr(unsafe.Pointer(&wc))); r == 0 {
		return fmt.Errorf("RegisterClassExW: %v", e)
	}

	style := uintptr(WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX)
	hwnd, _, e := pCreateWindowEx.Call(0, uintptr(unsafe.Pointer(class)), uintptr(unsafe.Pointer(ptr(appName))), style,
		CW_USEDEFAULT, CW_USEDEFAULT, 540, 560, 0, 0, hinst, 0)
	if hwnd == 0 {
		return fmt.Errorf("CreateWindowExW: %v", e)
	}
	a.hwnd = hwnd
	dark := int32(1)
	pDwmSetWindowAttribute.Call(hwnd, 20, uintptr(unsafe.Pointer(&dark)), unsafe.Sizeof(dark))

	a.brushBG, _, _ = pCreateSolidBrush.Call(rgb(8, 10, 11))
	a.brushPanel, _, _ = pCreateSolidBrush.Call(rgb(16, 19, 20))
	a.brushEdit, _, _ = pCreateSolidBrush.Call(rgb(22, 25, 26))
	a.fontTitle = createFont(24, 600)
	a.fontBody = createFont(18, 400)
	a.fontSmall = createFont(15, 400)

	// Pair code edit lives only on first-run/re-pair screen.
	editClass := ptr("EDIT")
	edit, _, _ := pCreateWindowEx.Call(0, uintptr(unsafe.Pointer(editClass)), uintptr(unsafe.Pointer(ptr(""))),
		WS_CHILD|WS_BORDER|ES_CENTER|ES_UPPERCASE, 85, 210, 370, 38, hwnd, 0, hinst, 0)
	a.edit = edit
	// Real Win32 child control for pairing. The 0.2 prototype drew the button
	// directly onto the parent window; failures were silent and could look like
	// a dead button. A child STATIC with SS_NOTIFY gives us reliable WM_COMMAND
	// click delivery while keeping the same minimalist appearance.
	staticClass := ptr("STATIC")
	pairBtn, _, _ := pCreateWindowEx.Call(0, uintptr(unsafe.Pointer(staticClass)), uintptr(unsafe.Pointer(ptr("PAIR & FINISH"))),
		WS_CHILD|SS_NOTIFY|SS_CENTER|SS_CENTERIMAGE, 85, 470, 370, 48, hwnd, ctrlPairFinish, hinst, 0)
	a.pairBtn = pairBtn
	if pairBtn != 0 {
		pSendMessage.Call(pairBtn, 0x0030 /* WM_SETFONT */, a.fontBody, 1)
	}
	a.loadIcons()
	a.addTray()
	a.installMouseHook()
	if a.paired {
		pShowWindow.Call(hwnd, SW_HIDE)
	} else {
		pShowWindow.Call(edit, SW_SHOW)
		if a.pairBtn != 0 {
			pShowWindow.Call(a.pairBtn, SW_SHOW)
		}
		pShowWindow.Call(hwnd, SW_SHOW)
		pUpdateWindow.Call(hwnd)
	}
	return nil
}

func createFont(px int32, weight int32) uintptr {
	face := ptr("Segoe UI")
	h, _, _ := pCreateFont.Call(uintptr(-px), 0, 0, 0, uintptr(weight), 0, 0, 0, 1, 0, 0, 5, 0, uintptr(unsafe.Pointer(face)))
	return h
}

func (a *winApp) cleanup() {
	a.stopHeartbeat()
	if a.hook != 0 {
		pUnhookWindowsHookEx.Call(a.hook)
	}
	a.deleteTray()
	for _, h := range []uintptr{a.iconGreen, a.iconRed, a.iconGray} {
		if h != 0 {
			pDestroyIcon.Call(h)
		}
	}
	for _, h := range []uintptr{a.brushBG, a.brushPanel, a.brushEdit, a.fontTitle, a.fontBody, a.fontSmall} {
		if h != 0 {
			pDeleteObject.Call(h)
		}
	}
}

func (a *winApp) messageLoop() error {
	var msg MSG
	for {
		r, _, e := pGetMessage.Call(uintptr(unsafe.Pointer(&msg)), 0, 0, 0)
		if int32(r) == -1 {
			return fmt.Errorf("GetMessageW: %v", e)
		}
		if r == 0 {
			return nil
		}
		pTranslateMessage.Call(uintptr(unsafe.Pointer(&msg)))
		pDispatchMessage.Call(uintptr(unsafe.Pointer(&msg)))
	}
}

func wndProc(hwnd uintptr, msg uint32, wParam, lParam uintptr) uintptr {
	a := currentApp
	if a == nil {
		r, _, _ := pDefWindowProc.Call(hwnd, uintptr(msg), wParam, lParam)
		return r
	}
	switch msg {
	case WM_PAINT:
		a.paint()
		return 0
	case WM_CLOSE:
		pShowWindow.Call(hwnd, SW_HIDE)
		return 0
	case WM_LBUTTONUP:
		x := int32(int16(lParam & 0xffff))
		y := int32(int16((lParam >> 16) & 0xffff))
		a.click(x, y)
		return 0
	case WM_CTLCOLORSTATIC, WM_CTLCOLOREDIT:
		hdc := wParam
		if lParam == a.pairBtn {
			a.mu.RLock()
			pairing, pairErr := a.pairing, a.pairError
			a.mu.RUnlock()
			color := rgb(91, 205, 74)
			if pairing {
				color = rgb(227, 190, 73)
			}
			if pairErr != "" {
				color = rgb(255, 76, 68)
			}
			pSetTextColor.Call(hdc, color)
			pSetBkColor.Call(hdc, rgb(16, 19, 20))
			return a.brushPanel
		}
		pSetTextColor.Call(hdc, rgb(235, 238, 240))
		pSetBkColor.Call(hdc, rgb(22, 25, 26))
		return a.brushEdit
	case WM_TRAY:
		switch uint32(lParam) {
		case WM_LBUTTONDBLCLK:
			a.showWindow()
		case WM_RBUTTONUP:
			a.showTrayMenu()
		}
		return 0
	case WM_STATE:
		a.refreshTray()
		pInvalidateRect.Call(hwnd, 0, 1)
		return 0
	case WM_COMMAND:
		a.handleMenu(uint16(wParam & 0xffff))
		return 0
	case WM_DESTROY:
		pPostQuitMessage.Call(0)
		return 0
	}
	r, _, _ := pDefWindowProc.Call(hwnd, uintptr(msg), wParam, lParam)
	return r
}

func (a *winApp) paint() {
	var ps PAINTSTRUCT
	hdc, _, _ := pBeginPaint.Call(a.hwnd, uintptr(unsafe.Pointer(&ps)))
	defer pEndPaint.Call(a.hwnd, uintptr(unsafe.Pointer(&ps)))
	var rc RECT
	pGetClientRect.Call(a.hwnd, uintptr(unsafe.Pointer(&rc)))
	pFillRect.Call(hdc, uintptr(unsafe.Pointer(&rc)), a.brushBG)
	pSetBkMode.Call(hdc, TRANSPARENT)

	a.mu.RLock()
	paired, connected, tx, cfg := a.paired, a.connected, a.transmitting, a.cfg
	a.mu.RUnlock()
	drawText(hdc, a.fontTitle, 26, 26, 500, 58, "LLB COMMAND RADIO", rgb(245, 247, 248), DT_LEFT|DT_VCENTER|DT_SINGLELINE)
	drawText(hdc, a.fontSmall, 28, 66, 500, 90, "minimal command uplink", rgb(119, 126, 130), DT_LEFT|DT_VCENTER|DT_SINGLELINE)

	panel := RECT{26, 108, 496, 180}
	pFillRect.Call(hdc, uintptr(unsafe.Pointer(&panel)), a.brushPanel)
	statusText, statusColor := "DISCONNECTED", rgb(125, 130, 133)
	if connected {
		statusText, statusColor = "STANDBY", rgb(91, 205, 74)
	}
	if tx {
		statusText, statusColor = "TRANSMITTING", rgb(255, 76, 68)
	}
	drawText(hdc, a.fontBody, 46, 122, 350, 154, statusText, statusColor, DT_LEFT|DT_VCENTER|DT_SINGLELINE)
	if paired {
		drawText(hdc, a.fontSmall, 46, 150, 440, 172, "paired · fail-closed radio gate", rgb(145, 150, 154), DT_LEFT|DT_VCENTER|DT_SINGLELINE)
	}

	if !paired {
		drawText(hdc, a.fontBody, 28, 182, 490, 208, "PAIR CODE", rgb(188, 193, 196), DT_LEFT|DT_VCENTER|DT_SINGLELINE)
		drawText(hdc, a.fontSmall, 28, 260, 490, 286, "RADIO KEY", rgb(188, 193, 196), DT_LEFT|DT_VCENTER|DT_SINGLELINE)
		drawChoice(hdc, a, 85, 296, 175, 342, "Mouse4", cfg.RadioKey == "Mouse4")
		drawChoice(hdc, a, 196, 296, 286, 342, "Mouse5", cfg.RadioKey != "Mouse4")
		drawText(hdc, a.fontSmall, 28, 362, 490, 388, "MODE", rgb(188, 193, 196), DT_LEFT|DT_VCENTER|DT_SINGLELINE)
		drawChoice(hdc, a, 85, 398, 190, 444, "Hold", cfg.RadioMode != "Toggle")
		drawChoice(hdc, a, 211, 398, 326, 444, "Toggle", cfg.RadioMode == "Toggle")
		a.mu.RLock()
		pairing, pairErr := a.pairing, a.pairError
		a.mu.RUnlock()
		if pairing {
			drawText(hdc, a.fontSmall, 300, 362, 490, 388, "PAIRING…", rgb(227, 190, 73), DT_LEFT|DT_VCENTER|DT_SINGLELINE)
		} else if pairErr != "" {
			drawText(hdc, a.fontSmall, 300, 362, 500, 388, pairErr, rgb(255, 76, 68), DT_LEFT|DT_VCENTER|DT_SINGLELINE)
		}
	} else {
		drawText(hdc, a.fontSmall, 28, 210, 490, 236, "RADIO KEY", rgb(188, 193, 196), DT_LEFT|DT_VCENTER|DT_SINGLELINE)
		drawChoice(hdc, a, 85, 248, 175, 294, "Mouse4", cfg.RadioKey == "Mouse4")
		drawChoice(hdc, a, 196, 248, 286, 294, "Mouse5", cfg.RadioKey != "Mouse4")
		drawText(hdc, a.fontSmall, 28, 320, 490, 346, "COMMAND MODE", rgb(188, 193, 196), DT_LEFT|DT_VCENTER|DT_SINGLELINE)
		drawChoice(hdc, a, 85, 358, 190, 404, "Hold", cfg.RadioMode != "Toggle")
		drawChoice(hdc, a, 211, 358, 326, 404, "Toggle", cfg.RadioMode == "Toggle")
		note := "Free Mic default: talk to team normally. Hold radio key only when talking to Shotcaller."
		if cfg.VoiceMode == "PTT" {
			note = "Discord PTT mode: radio key must also be a Discord PTT key."
		}
		drawText(hdc, a.fontSmall, 32, 438, 490, 505, note, rgb(145, 150, 154), DT_LEFT)
	}
}

func drawText(hdc, font uintptr, l, t, r, b int32, text string, color uintptr, format uintptr) {
	old, _, _ := pSelectObject.Call(hdc, font)
	defer pSelectObject.Call(hdc, old)
	pSetTextColor.Call(hdc, color)
	rc := RECT{l, t, r, b}
	u, _ := syscall.UTF16FromString(text)
	pDrawText.Call(hdc, uintptr(unsafe.Pointer(&u[0])), uintptr(len(u)-1), uintptr(unsafe.Pointer(&rc)), format)
}
func drawChoice(hdc uintptr, a *winApp, l, t, r, b int32, text string, active bool) {
	brush := a.brushPanel
	pFillRect.Call(hdc, uintptr(unsafe.Pointer(&RECT{l, t, r, b})), brush)
	c := rgb(185, 190, 193)
	if active {
		c = rgb(91, 205, 74)
	}
	drawText(hdc, a.fontSmall, l, t, r, b, text, c, DT_CENTER|DT_VCENTER|DT_SINGLELINE)
}
func drawButton(hdc uintptr, a *winApp, l, t, r, b int32, text string, color uintptr) {
	pFillRect.Call(hdc, uintptr(unsafe.Pointer(&RECT{l, t, r, b})), a.brushPanel)
	drawText(hdc, a.fontBody, l, t, r, b, text, color, DT_CENTER|DT_VCENTER|DT_SINGLELINE)
}

func (a *winApp) click(x, y int32) {
	a.mu.RLock()
	paired := a.paired
	a.mu.RUnlock()
	if !paired {
		switch {
		case inRect(x, y, 85, 296, 175, 342):
			a.setRadioKey("Mouse4")
		case inRect(x, y, 196, 296, 286, 342):
			a.setRadioKey("Mouse5")
		case inRect(x, y, 85, 398, 190, 444):
			a.setRadioMode("Hold")
		case inRect(x, y, 211, 398, 326, 444):
			a.setRadioMode("Toggle")
		}
	} else {
		switch {
		case inRect(x, y, 85, 248, 175, 294):
			a.setRadioKey("Mouse4")
		case inRect(x, y, 196, 248, 286, 294):
			a.setRadioKey("Mouse5")
		case inRect(x, y, 85, 358, 190, 404):
			a.setRadioMode("Hold")
		case inRect(x, y, 211, 358, 326, 404):
			a.setRadioMode("Toggle")
		}
	}
}
func inRect(x, y, l, t, r, b int32) bool { return x >= l && x <= r && y >= t && y <= b }

func (a *winApp) pairFromEdit() {
	a.mu.Lock()
	if a.pairing {
		a.mu.Unlock()
		return
	}
	a.pairing = true
	a.pairError = ""
	a.mu.Unlock()
	if a.pairBtn != 0 {
		pSetWindowText.Call(a.pairBtn, uintptr(unsafe.Pointer(ptr("PAIRING…"))))
	}
	a.postState()

	buf := make([]uint16, 64)
	n, _, _ := pGetWindowText.Call(a.edit, uintptr(unsafe.Pointer(&buf[0])), uintptr(len(buf)))
	code := syscall.UTF16ToString(buf[:n])
	if strings.TrimSpace(code) == "" {
		a.finishPairError("ENTER PAIR CODE")
		return
	}
	ctx, cancel := context.WithTimeout(context.Background(), 4*time.Second)
	defer cancel()
	res, err := a.api.Pair(ctx, code, "Windows helper")
	if err != nil {
		msg := "PAIR FAILED"
		errText := strings.ToLower(err.Error())
		if strings.Contains(errText, "401") || strings.Contains(errText, "expired") || strings.Contains(errText, "invalid") {
			msg = "CODE EXPIRED / INVALID"
		} else if strings.Contains(errText, "connect") || strings.Contains(errText, "refused") || strings.Contains(errText, "timeout") || strings.Contains(errText, "deadline") {
			msg = "SERVER UNREACHABLE"
		}
		a.finishPairError(msg)
		return
	}
	a.mu.Lock()
	a.cfg.DeviceToken = res.Token
	a.cfg.GuildID = res.GuildID
	a.cfg.UserID = res.UserID
	a.paired = true
	a.pairing = false
	a.pairError = ""
	cfg := a.cfg
	a.mu.Unlock()
	a.api.SetToken(res.Token)
	_ = saveConfig(cfg)
	a.setConnected(true)
	pShowWindow.Call(a.edit, SW_HIDE)
	if a.pairBtn != 0 {
		pShowWindow.Call(a.pairBtn, SW_HIDE)
	}
	pShowWindow.Call(a.hwnd, SW_HIDE)
}

func (a *winApp) finishPairError(msg string) {
	a.mu.Lock()
	a.pairing = false
	a.pairError = msg
	a.connected = false
	a.transmitting = false
	a.mu.Unlock()
	if a.pairBtn != 0 {
		pSetWindowText.Call(a.pairBtn, uintptr(unsafe.Pointer(ptr("TRY AGAIN"))))
	}
	a.postState()
}

func (a *winApp) setRadioKey(k string) {
	a.mu.Lock()
	a.cfg.RadioKey = k
	cfg := a.cfg
	a.mu.Unlock()
	_ = saveConfig(cfg)
	a.postState()
}
func (a *winApp) setRadioMode(m string) {
	a.mu.Lock()
	a.cfg.RadioMode = m
	cfg := a.cfg
	a.mu.Unlock()
	_ = saveConfig(cfg)
	a.postState()
}
func (a *winApp) setVoiceMode(m string) {
	a.mu.Lock()
	a.cfg.VoiceMode = m
	cfg := a.cfg
	a.mu.Unlock()
	_ = saveConfig(cfg)
	a.postState()
}

func (a *winApp) installMouseHook() {
	cb := syscall.NewCallback(mouseHookProc)
	hinst, _, _ := pGetModuleHandle.Call(0)
	h, _, _ := pSetWindowsHookEx.Call(WH_MOUSE_LL, cb, hinst, 0)
	a.hook = h
}
func mouseHookProc(nCode int, wParam, lParam uintptr) uintptr {
	a := currentApp
	if nCode == HC_ACTION && a != nil && lParam != 0 {
		s := (*MSLLHOOKSTRUCT)(unsafe.Pointer(lParam))
		xb := uint16((s.MouseData >> 16) & 0xffff)
		a.mu.RLock()
		key := a.cfg.RadioKey
		a.mu.RUnlock()
		matched := (key == "Mouse4" && xb == XBUTTON1) || (key != "Mouse4" && xb == XBUTTON2)
		if matched {
			if uint32(wParam) == WM_XBUTTONDOWN {
				select {
				case a.radioEvents <- true:
				default:
				}
			}
			if uint32(wParam) == WM_XBUTTONUP {
				select {
				case a.radioEvents <- false:
				default:
				}
			}
		}
	}
	r, _, _ := pCallNextHookEx.Call(0, uintptr(nCode), wParam, lParam)
	return r
}

func (a *winApp) radioWorker() {
	for down := range a.radioEvents {
		a.mu.RLock()
		paired := a.paired
		mode := a.cfg.RadioMode
		current := a.transmitting
		a.mu.RUnlock()
		if !paired {
			continue
		}
		var target bool
		if mode == "Toggle" {
			if !down {
				continue
			}
			target = !current
		} else {
			target = down
		}
		if target == current {
			continue
		}
		ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
		err := a.api.SetState(ctx, target)
		cancel()
		if err != nil {
			a.stopHeartbeat()
			a.setState(false, false)
			continue
		}
		if target {
			a.startHeartbeat()
		} else {
			a.stopHeartbeat()
		}
		a.setState(true, target)
	}
}
func (a *winApp) startHeartbeat() {
	a.stopHeartbeat()
	ctx, cancel := context.WithCancel(context.Background())
	a.mu.Lock()
	a.heartbeatCancel = cancel
	a.mu.Unlock()
	go func() {
		t := time.NewTicker(250 * time.Millisecond)
		defer t.Stop()
		for {
			select {
			case <-ctx.Done():
				return
			case <-t.C:
				c, cc := context.WithTimeout(context.Background(), 1500*time.Millisecond)
				err := a.api.Heartbeat(c)
				cc()
				if err != nil {
					a.setState(false, false)
					return
				}
			}
		}
	}()
}
func (a *winApp) stopHeartbeat() {
	a.mu.Lock()
	c := a.heartbeatCancel
	a.heartbeatCancel = nil
	a.mu.Unlock()
	if c != nil {
		c()
	}
}
func (a *winApp) probeLoop() {
	t := time.NewTicker(3 * time.Second)
	defer t.Stop()
	for range t.C {
		a.mu.RLock()
		paired, tx, quit := a.paired, a.transmitting, a.quitting
		a.mu.RUnlock()
		if quit {
			return
		}
		if !paired || tx {
			continue
		}
		ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
		err := a.api.Probe(ctx)
		cancel()
		a.setConnected(err == nil)
	}
}
func (a *winApp) setConnected(v bool) {
	a.mu.Lock()
	a.connected = v
	if !v {
		a.transmitting = false
	}
	a.mu.Unlock()
	a.postState()
}
func (a *winApp) setState(conn, tx bool) {
	a.mu.Lock()
	a.connected = conn
	a.transmitting = tx
	a.mu.Unlock()
	a.postState()
}
func (a *winApp) postState() {
	if a.hwnd != 0 {
		pPostMessage.Call(a.hwnd, WM_STATE, 0, 0)
	}
}

func (a *winApp) loadIcons() {
	dir := filepath.Join(os.TempDir(), "LLBCommandRadio-icons")
	_ = os.MkdirAll(dir, 0o700)
	load := func(name string) uintptr {
		b, _ := fs.ReadFile(iconFS, "assets/"+name)
		p := filepath.Join(dir, name)
		_ = os.WriteFile(p, b, 0o600)
		h, _, _ := pLoadImage.Call(0, uintptr(unsafe.Pointer(ptr(p))), IMAGE_ICON, 0, 0, LR_LOADFROMFILE|LR_DEFAULTSIZE)
		return h
	}
	a.iconGreen = load("green.ico")
	a.iconRed = load("red.ico")
	a.iconGray = load("gray.ico")
}
func (a *winApp) trayData(icon uintptr) NOTIFYICONDATA {
	var n NOTIFYICONDATA
	n.CbSize = uint32(unsafe.Sizeof(n))
	n.HWnd = a.hwnd
	n.UID = 1
	n.UFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP
	n.UCallbackMessage = WM_TRAY
	n.HIcon = icon
	a.mu.RLock()
	conn, tx := a.connected, a.transmitting
	a.mu.RUnlock()
	tip := "LLB Command Radio — disconnected"
	if conn {
		tip = "LLB Command Radio — standby"
	}
	if tx {
		tip = "LLB Command Radio — transmitting"
	}
	setTip(n.SzTip[:], tip)
	return n
}
func (a *winApp) addTray() {
	n := a.trayData(a.iconGray)
	pShellNotifyIcon.Call(NIM_ADD, uintptr(unsafe.Pointer(&n)))
}
func (a *winApp) refreshTray() {
	a.mu.RLock()
	conn, tx := a.connected, a.transmitting
	a.mu.RUnlock()
	icon := a.iconGray
	if conn {
		icon = a.iconGreen
	}
	if tx {
		icon = a.iconRed
	}
	n := a.trayData(icon)
	pShellNotifyIcon.Call(NIM_MODIFY, uintptr(unsafe.Pointer(&n)))
}
func (a *winApp) deleteTray() {
	if a.hwnd == 0 {
		return
	}
	n := a.trayData(a.iconGray)
	pShellNotifyIcon.Call(NIM_DELETE, uintptr(unsafe.Pointer(&n)))
}
func (a *winApp) showWindow() {
	pShowWindow.Call(a.hwnd, SW_SHOWNORMAL)
	pSetForegroundWindow.Call(a.hwnd)
	pInvalidateRect.Call(a.hwnd, 0, 1)
}

func (a *winApp) showTrayMenu() {
	menu, _, _ := pCreatePopupMenu.Call()
	if menu == 0 {
		return
	}
	defer pDestroyMenu.Call(menu)
	appendMenu(menu, MF_STRING, cmdOpen, "Open")
	appendMenu(menu, MF_SEPARATOR, 0, "")
	a.mu.RLock()
	cfg := a.cfg
	a.mu.RUnlock()
	f := uintptr(MF_STRING)
	if cfg.RadioKey == "Mouse4" {
		f |= MF_CHECKED
	}
	appendMenu(menu, f, cmdKeyMouse4, "Radio key: Mouse4")
	f = MF_STRING
	if cfg.RadioKey != "Mouse4" {
		f |= MF_CHECKED
	}
	appendMenu(menu, f, cmdKeyMouse5, "Radio key: Mouse5")
	appendMenu(menu, MF_SEPARATOR, 0, "")
	f = MF_STRING
	if cfg.RadioMode != "Toggle" {
		f |= MF_CHECKED
	}
	appendMenu(menu, f, cmdModeHold, "Hold to transmit")
	f = MF_STRING
	if cfg.RadioMode == "Toggle" {
		f |= MF_CHECKED
	}
	appendMenu(menu, f, cmdModeToggle, "Toggle radio")
	appendMenu(menu, MF_SEPARATOR, 0, "")
	f = MF_STRING
	if cfg.VoiceMode != "PTT" {
		f |= MF_CHECKED
	}
	appendMenu(menu, f, cmdVoiceOpen, "Local voice: Free Mic")
	f = MF_STRING
	if cfg.VoiceMode == "PTT" {
		f |= MF_CHECKED
	}
	appendMenu(menu, f, cmdVoicePTT, "Local voice: Discord PTT")
	appendMenu(menu, MF_SEPARATOR, 0, "")
	appendMenu(menu, MF_STRING, cmdRepair, "Re-pair")
	appendMenu(menu, MF_STRING, cmdUninstall, "Uninstall")
	appendMenu(menu, MF_STRING, cmdExit, "Exit")
	var pt POINT
	pGetCursorPos.Call(uintptr(unsafe.Pointer(&pt)))
	pSetForegroundWindow.Call(a.hwnd)
	id, _, _ := pTrackPopupMenu.Call(menu, TPM_RIGHTBUTTON|TPM_RETURNCMD, uintptr(pt.X), uintptr(pt.Y), 0, a.hwnd, 0)
	if id != 0 {
		a.handleMenu(uint16(id))
	}
}
func appendMenu(menu, flags, id uintptr, text string) {
	var p uintptr
	if text != "" {
		p = uintptr(unsafe.Pointer(ptr(text)))
	}
	pAppendMenu.Call(menu, flags, id, p)
}
func (a *winApp) handleMenu(id uint16) {
	switch uintptr(id) {
	case ctrlPairFinish:
		go a.pairFromEdit()
	case cmdOpen:
		a.showWindow()
	case cmdKeyMouse4:
		a.setRadioKey("Mouse4")
	case cmdKeyMouse5:
		a.setRadioKey("Mouse5")
	case cmdModeHold:
		a.setRadioMode("Hold")
	case cmdModeToggle:
		a.setRadioMode("Toggle")
	case cmdVoiceOpen:
		a.setVoiceMode("OpenMic")
	case cmdVoicePTT:
		a.setVoiceMode("PTT")
	case cmdRepair:
		a.mu.Lock()
		a.paired = false
		a.pairing = false
		a.pairError = ""
		a.connected = false
		a.transmitting = false
		a.cfg.DeviceToken = ""
		cfg := a.cfg
		a.mu.Unlock()
		a.api.SetToken("")
		_ = saveConfig(cfg)
		pShowWindow.Call(a.edit, SW_SHOW)
		if a.pairBtn != 0 {
			pSetWindowText.Call(a.pairBtn, uintptr(unsafe.Pointer(ptr("PAIR & FINISH"))))
			pShowWindow.Call(a.pairBtn, SW_SHOW)
		}
		a.showWindow()
	case cmdUninstall:
		r, _, _ := pMessageBox.Call(a.hwnd, uintptr(unsafe.Pointer(ptr("Uninstall LLB Command Radio?"))), uintptr(unsafe.Pointer(ptr(appName))), MB_YESNO|MB_ICONQUESTION)
		if r == IDYES {
			a.quitting = true
			a.deleteTray()
			_ = removeInstallRegistration()
			scheduleSelfDelete()
			pPostQuitMessage.Call(0)
		}
	case cmdExit:
		a.mu.Lock()
		a.quitting = true
		a.mu.Unlock()
		pPostQuitMessage.Call(0)
	}
}
