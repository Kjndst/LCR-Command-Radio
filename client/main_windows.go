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

//go:embed assets/*.ico assets/lcr-banner.png
var assetFS embed.FS

const (
	appName     = "LCR"
	appSubtitle = "Linh Lan Bang – Command Radio"
	windowTitle = "LCR — Linh Lan Bang – Command Radio"
	className   = "LCRCommandRadioWindow"

	WM_DESTROY        = 0x0002
	WM_CLOSE          = 0x0010
	WM_PAINT          = 0x000F
	WM_LBUTTONUP      = 0x0202
	WM_LBUTTONDOWN    = 0x0201
	WM_MOUSEMOVE      = 0x0200
	WM_MOUSELEAVE     = 0x02A3
	WM_ERASEBKGND     = 0x0014
	WM_NCHITTEST      = 0x0084
	WM_KEYDOWN        = 0x0100
	WM_KEYUP          = 0x0101
	WM_SYSKEYDOWN     = 0x0104
	WM_SYSKEYUP       = 0x0105
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

	TME_LEAVE = 0x00000002

	WH_KEYBOARD_LL = 13
	WH_MOUSE_LL    = 14
	HC_ACTION      = 0
	XBUTTON1       = 1
	XBUTTON2       = 2
	VK_ESCAPE      = 0x1B

	HTCLIENT  = 1
	HTCAPTION = 2

	MONITOR_DEFAULTTONEAREST = 2
	SWP_NOSIZE               = 0x0001
	SWP_NOZORDER             = 0x0004

	WS_POPUP        = 0x80000000
	WS_SYSMENU      = 0x00080000
	WS_CHILD        = 0x40000000
	WS_BORDER       = 0x00800000
	WS_EX_APPWINDOW = 0x00040000
	ES_CENTER       = 0x0001
	ES_MULTILINE    = 0x0004
	ES_UPPERCASE    = 0x0008
	ES_AUTOVSCROLL  = 0x0040
	EM_SETRECTNP    = 0x00B4
	WM_SETFONT      = 0x0030

	SW_HIDE       = 0
	SW_SHOW       = 5
	SW_SHOWNORMAL = 1

	CW_USEDEFAULT = 0x80000000
	IDC_ARROW     = 32512
	SRCCOPY       = 0x00CC0020

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
	DT_RIGHT      = 0x00000002
	DT_CENTER     = 0x00000001
	DT_VCENTER    = 0x00000004
	DT_SINGLELINE = 0x00000020

	MB_YESNO           = 0x00000004
	MB_ICONQUESTION    = 0x00000020
	MB_OK              = 0x00000000
	MB_ICONEXCLAMATION = 0x00000030
	MB_ICONASTERISK    = 0x00000040
	IDYES              = 6
	DT_END_ELLIPSIS    = 0x00008000

	cmdOpen      = 1001
	cmdChangeKey = 1002
	cmdReconnect = 1005
	cmdUnpair    = 1006
	cmdUninstall = 1007
	cmdExit      = 1008
)

const (
	controlNone controlID = iota
	controlPair
	controlChangeKey
	controlMinimize
	controlClose
	controlReconnect
	controlUnpair
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
type KBDLLHOOKSTRUCT struct {
	VkCode      uint32
	ScanCode    uint32
	Flags       uint32
	Time        uint32
	DwExtraInfo uintptr
}
type TRACKMOUSEEVENT struct {
	CbSize      uint32
	DwFlags     uint32
	HWndTrack   uintptr
	DwHoverTime uint32
}
type MONITORINFO struct {
	CbSize    uint32
	RcMonitor RECT
	RcWork    RECT
	DwFlags   uint32
}
type GdiplusStartupInput struct {
	GdiplusVersion           uint32
	DebugEventCallback       uintptr
	SuppressBackgroundThread int32
	SuppressExternalCodecs   int32
}
type controlID uint8

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
	pGetWindowRect       = user32.NewProc("GetWindowRect")
	pMonitorFromWindow   = user32.NewProc("MonitorFromWindow")
	pGetMonitorInfo      = user32.NewProc("GetMonitorInfoW")
	pMessageBeep         = user32.NewProc("MessageBeep")
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
	pScreenToClient      = user32.NewProc("ScreenToClient")
	pGetKeyNameText      = user32.NewProc("GetKeyNameTextW")
	pTrackMouseEvent     = user32.NewProc("TrackMouseEvent")
	pSetCapture          = user32.NewProc("SetCapture")
	pReleaseCapture      = user32.NewProc("ReleaseCapture")

	pGetModuleHandle = kernel32.NewProc("GetModuleHandleW")

	pCreateSolidBrush       = gdi32.NewProc("CreateSolidBrush")
	pDeleteObject           = gdi32.NewProc("DeleteObject")
	pDeleteDC               = gdi32.NewProc("DeleteDC")
	pCreateCompatibleDC     = gdi32.NewProc("CreateCompatibleDC")
	pCreateCompatibleBitmap = gdi32.NewProc("CreateCompatibleBitmap")
	pBitBlt                 = gdi32.NewProc("BitBlt")
	pFillRect               = user32.NewProc("FillRect")
	pSetTextColor           = gdi32.NewProc("SetTextColor")
	pSetBkColor             = gdi32.NewProc("SetBkColor")
	pSetBkMode              = gdi32.NewProc("SetBkMode")
	pCreateFont             = gdi32.NewProc("CreateFontW")
	pSelectObject           = gdi32.NewProc("SelectObject")
	pDrawText               = user32.NewProc("DrawTextW")

	pShellNotifyIcon       = shell32.NewProc("Shell_NotifyIconW")
	pDwmSetWindowAttribute = dwmapi.NewProc("DwmSetWindowAttribute")

	gdiplus                = syscall.NewLazyDLL("gdiplus.dll")
	pGdiplusStartup        = gdiplus.NewProc("GdiplusStartup")
	pGdiplusShutdown       = gdiplus.NewProc("GdiplusShutdown")
	pGdipLoadImageFromFile = gdiplus.NewProc("GdipLoadImageFromFile")
	pGdipDisposeImage      = gdiplus.NewProc("GdipDisposeImage")
	pGdipCreateFromHDC     = gdiplus.NewProc("GdipCreateFromHDC")
	pGdipDeleteGraphics    = gdiplus.NewProc("GdipDeleteGraphics")
	pGdipDrawImageRectI    = gdiplus.NewProc("GdipDrawImageRectI")
)

func rgb(r, g, b byte) uintptr      { return uintptr(r) | uintptr(g)<<8 | uintptr(b)<<16 }
func ptr(s string) *uint16          { p, _ := syscall.UTF16PtrFromString(s); return p }
func setTip(dst []uint16, s string) { u, _ := syscall.UTF16FromString(s); copy(dst, u) }

var currentApp *winApp

type winApp struct {
	mu                                             sync.RWMutex
	cfg                                            AppConfig
	api                                            *RadioAPI
	hwnd                                           uintptr
	edit                                           uintptr
	mouseHook, keyboardHook                        uintptr
	paired                                         bool
	pairing                                        bool
	pairError                                      string
	notice                                         string
	noticeKind                                     noticeKind
	connected                                      bool
	transmitting                                   bool
	capturing                                      bool
	quitting                                       bool
	hovered, pressed                               controlID
	heartbeatCancel                                context.CancelFunc
	radioEvents                                    chan bool
	keyEvents                                      chan keyCaptureEvent
	iconGreen, iconRed, iconGray                   uintptr
	brushBG, brushPanel, brushEdit                 uintptr
	brushLine, brushLineDim                        uintptr
	brushButton, brushButtonBusy, brushButtonError uintptr
	fontTitle, fontBody, fontSmall                 uintptr
	gdiplusToken, bannerImage                      uintptr
}

type noticeKind uint8

const (
	noticeInfo noticeKind = iota
	noticeSuccess
	noticeWarning
)

type keyCaptureEvent struct {
	kind     string
	name     string
	vk       uint32
	scanCode uint32
	flags    uint32
}

func run(args []string) error {
	runtime.LockOSThread()
	if hasArg(args, "--cleanup") {
		return performCleanup(args)
	}
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
	a := &winApp{cfg: loadConfig(), radioEvents: make(chan bool, 16), keyEvents: make(chan keyCaptureEvent, 8), notice: "READY FOR PAIRING", noticeKind: noticeInfo}
	a.api = NewRadioAPI(a.cfg.ServerURL, a.cfg.DeviceToken)
	a.paired = a.cfg.DeviceToken != ""
	currentApp = a
	if err := a.init(); err != nil {
		return err
	}
	defer a.cleanup()
	go a.radioWorker()
	go a.keyCaptureWorker()
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
	appIcon, _, _ := pLoadImage.Call(hinst, 1, IMAGE_ICON, 0, 0, LR_DEFAULTSIZE)
	wc := WNDCLASSEX{CbSize: uint32(unsafe.Sizeof(WNDCLASSEX{})), LpfnWndProc: syscall.NewCallback(wndProc), HInstance: hinst, HCursor: cursor, HIcon: appIcon, HIconSm: appIcon, LpszClassName: class}
	if r, _, e := pRegisterClassEx.Call(uintptr(unsafe.Pointer(&wc))); r == 0 {
		return fmt.Errorf("RegisterClassExW: %v", e)
	}

	style := uintptr(WS_POPUP | WS_SYSMENU)
	hwnd, _, e := pCreateWindowEx.Call(WS_EX_APPWINDOW, uintptr(unsafe.Pointer(class)), uintptr(unsafe.Pointer(ptr(windowTitle))), style,
		CW_USEDEFAULT, CW_USEDEFAULT, 540, 610, 0, 0, hinst, 0)
	if hwnd == 0 {
		return fmt.Errorf("CreateWindowExW: %v", e)
	}
	a.hwnd = hwnd
	a.brushBG, _, _ = pCreateSolidBrush.Call(rgb(5, 6, 7))
	a.brushPanel, _, _ = pCreateSolidBrush.Call(rgb(14, 12, 13))
	a.brushEdit, _, _ = pCreateSolidBrush.Call(rgb(22, 17, 18))
	a.brushLine, _, _ = pCreateSolidBrush.Call(rgb(225, 35, 29))
	a.brushLineDim, _, _ = pCreateSolidBrush.Call(rgb(92, 23, 22))
	a.brushButton, _, _ = pCreateSolidBrush.Call(rgb(49, 9, 9))
	a.brushButtonBusy, _, _ = pCreateSolidBrush.Call(rgb(62, 43, 9))
	a.brushButtonError, _, _ = pCreateSolidBrush.Call(rgb(67, 12, 12))
	a.fontTitle = createFont(28, 700, "Bahnschrift SemiCondensed")
	a.fontBody = createFont(18, 600, "Bahnschrift SemiCondensed")
	a.fontSmall = createFont(14, 500, "Bahnschrift SemiCondensed")

	// Pair code edit lives only on first-run/re-pair screen.
	editClass := ptr("EDIT")
	edit, _, _ := pCreateWindowEx.Call(0, uintptr(unsafe.Pointer(editClass)), uintptr(unsafe.Pointer(ptr(""))),
		WS_CHILD|WS_BORDER|ES_CENTER|ES_MULTILINE|ES_AUTOVSCROLL|ES_UPPERCASE, 85, 328, 370, 38, hwnd, 0, hinst, 0)
	a.edit = edit
	pSendMessage.Call(edit, WM_SETFONT, a.fontBody, 1)
	editTextRect := RECT{Left: 8, Top: 9, Right: 362, Bottom: 29}
	pSendMessage.Call(edit, EM_SETRECTNP, 0, uintptr(unsafe.Pointer(&editTextRect)))
	a.loadIcons()
	a.loadBanner()
	a.addTray()
	a.installInputHooks()
	if !a.paired {
		pShowWindow.Call(edit, SW_SHOW)
	}
	a.showWindow()
	pUpdateWindow.Call(hwnd)
	return nil
}

func createFont(px int32, weight int32, family string) uintptr {
	face := ptr(family)
	h, _, _ := pCreateFont.Call(uintptr(-px), 0, 0, 0, uintptr(weight), 0, 0, 0, 1, 0, 0, 5, 0, uintptr(unsafe.Pointer(face)))
	return h
}

func (a *winApp) cleanup() {
	a.stopHeartbeat()
	for _, hook := range []uintptr{a.mouseHook, a.keyboardHook} {
		if hook != 0 {
			pUnhookWindowsHookEx.Call(hook)
		}
	}
	if a.bannerImage != 0 {
		pGdipDisposeImage.Call(a.bannerImage)
	}
	if a.gdiplusToken != 0 {
		pGdiplusShutdown.Call(a.gdiplusToken)
	}
	a.deleteTray()
	for _, h := range []uintptr{a.iconGreen, a.iconRed, a.iconGray} {
		if h != 0 {
			pDestroyIcon.Call(h)
		}
	}
	for _, h := range []uintptr{a.brushBG, a.brushPanel, a.brushEdit, a.brushLine, a.brushLineDim, a.brushButton, a.brushButtonBusy, a.brushButtonError, a.fontTitle, a.fontBody, a.fontSmall} {
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
	case WM_ERASEBKGND:
		// paint() owns the complete client area through a memory buffer.
		return 1
	case WM_NCHITTEST:
		return a.hitTest(lParam)
	case WM_PAINT:
		a.paint()
		return 0
	case WM_CLOSE:
		pShowWindow.Call(hwnd, SW_HIDE)
		return 0
	case WM_LBUTTONUP:
		x := int32(int16(lParam & 0xffff))
		y := int32(int16((lParam >> 16) & 0xffff))
		a.releaseControl(x, y)
		return 0
	case WM_LBUTTONDOWN:
		x := int32(int16(lParam & 0xffff))
		y := int32(int16((lParam >> 16) & 0xffff))
		a.pressControl(x, y)
		return 0
	case WM_MOUSEMOVE:
		x := int32(int16(lParam & 0xffff))
		y := int32(int16((lParam >> 16) & 0xffff))
		a.hoverControl(x, y)
		return 0
	case WM_MOUSELEAVE:
		a.setHovered(controlNone)
		return 0
	case WM_CTLCOLORSTATIC, WM_CTLCOLOREDIT:
		hdc := wParam
		pSetTextColor.Call(hdc, rgb(244, 232, 229))
		pSetBkColor.Call(hdc, rgb(22, 17, 18))
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
		pInvalidateRect.Call(hwnd, 0, 0)
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
	paintDC := hdc
	var rc RECT
	pGetClientRect.Call(a.hwnd, uintptr(unsafe.Pointer(&rc)))
	memDC, _, _ := pCreateCompatibleDC.Call(paintDC)
	if memDC == 0 {
		return
	}
	defer pDeleteDC.Call(memDC)
	bitmap, _, _ := pCreateCompatibleBitmap.Call(paintDC, uintptr(rc.Right-rc.Left), uintptr(rc.Bottom-rc.Top))
	if bitmap == 0 {
		return
	}
	oldBitmap, _, _ := pSelectObject.Call(memDC, bitmap)
	defer func() {
		pBitBlt.Call(paintDC, 0, 0, uintptr(rc.Right-rc.Left), uintptr(rc.Bottom-rc.Top), memDC, 0, 0, SRCCOPY)
		pSelectObject.Call(memDC, oldBitmap)
		pDeleteObject.Call(bitmap)
	}()
	hdc = memDC
	pFillRect.Call(hdc, uintptr(unsafe.Pointer(&rc)), a.brushBG)
	pSetBkMode.Call(hdc, TRANSPARENT)

	a.mu.RLock()
	paired, connected, tx, pairing, pairErr, capturing, notice, noticeKind, cfg := a.paired, a.connected, a.transmitting, a.pairing, a.pairError, a.capturing, a.notice, a.noticeKind, a.cfg
	a.mu.RUnlock()
	drawHeader(hdc, a)

	panel := RECT{26, 170, 514, 238}
	drawTechFrame(hdc, a, panel, true)
	statusText, statusColor := "DISCONNECTED", rgb(125, 130, 133)
	if connected {
		statusText, statusColor = "STANDBY", rgb(91, 205, 74)
	}
	if tx {
		statusText, statusColor = "TRANSMITTING", rgb(255, 76, 68)
		drawTransmitSignal(hdc, a, panel)
	}
	drawText(hdc, a.fontSmall, 46, 178, 480, 196, "COMMAND RADIO", rgb(175, 67, 61), DT_LEFT|DT_VCENTER|DT_SINGLELINE)
	drawText(hdc, a.fontBody, 46, 196, 350, 226, statusText, statusColor, DT_LEFT|DT_VCENTER|DT_SINGLELINE)
	statusDetail := "NOT CONNECTED"
	if paired && connected {
		statusDetail = "READY"
	}
	if tx {
		statusDetail = "COMMAND LIVE"
	}
	drawText(hdc, a.fontSmall, 290, 198, 490, 224, statusDetail, statusColor, DT_RIGHT|DT_VCENTER|DT_SINGLELINE)

	if !paired {
		drawSectionLabel(hdc, a, 28, 258, "CONNECT TO COMMAND")
		drawText(hdc, a.fontSmall, 85, 294, 455, 314, "PAIR CODE", rgb(185, 145, 141), DT_LEFT|DT_VCENTER|DT_SINGLELINE)
		drawRadioKey(hdc, a, cfg, capturing, 386)
		buttonText := "CONNECT RADIO"
		if pairing {
			buttonText = "CONNECTING…"
		} else if pairErr != "" {
			buttonText = "TRY AGAIN"
		}
		drawActionButton(hdc, a, controlPair, RECT{85, 486, 455, 536}, buttonText, pairing)
	} else {
		section := "READY FOR COMMAND"
		if !connected {
			section = "CONNECTION LOST"
		}
		drawSectionLabel(hdc, a, 28, 258, section)
		headline := "HOLD " + cfg.RadioKey + " TO TALK"
		if !connected {
			headline = "RECONNECT TO COMMAND"
		}
		drawText(hdc, a.fontBody, 85, 294, 455, 326, headline, statusColor, DT_CENTER|DT_VCENTER|DT_SINGLELINE)
		drawRadioKey(hdc, a, cfg, capturing, 350)
		if connected {
			drawActionButton(hdc, a, controlMinimize, RECT{85, 452, 263, 506}, "MINIMIZE", false)
			drawActionButton(hdc, a, controlUnpair, RECT{277, 452, 455, 506}, "UNPAIR", false)
		} else {
			drawActionButton(hdc, a, controlReconnect, RECT{85, 452, 263, 506}, "RECONNECT", false)
			drawActionButton(hdc, a, controlUnpair, RECT{277, 452, 455, 506}, "UNPAIR", false)
		}
	}
	drawTerminalStatus(hdc, a, notice, noticeKind)
}

func drawHeader(hdc uintptr, a *winApp) {
	a.drawBanner(hdc)
	drawWindowButton(hdc, a, controlClose, RECT{490, 6, 520, 28}, "×")
}

func drawRadioKey(hdc uintptr, a *winApp, cfg AppConfig, capturing bool, top int32) {
	drawText(hdc, a.fontSmall, 85, top, 455, top+20, "RADIO KEY", rgb(185, 145, 141), DT_LEFT|DT_VCENTER|DT_SINGLELINE)
	keyText := cfg.RadioKey
	if capturing {
		keyText = "WAITING..."
	}
	drawTechFrame(hdc, a, RECT{85, top + 26, 300, top + 72}, !capturing)
	drawText(hdc, a.fontBody, 97, top+26, 288, top+72, keyText, rgb(255, 82, 67), DT_CENTER|DT_VCENTER|DT_SINGLELINE)
	drawActionButton(hdc, a, controlChangeKey, RECT{315, top + 26, 455, top + 72}, "CHANGE", capturing)
}

func drawTerminalStatus(hdc uintptr, a *winApp, text string, kind noticeKind) {
	panel := RECT{60, 552, 480, 584}
	drawTechFrame(hdc, a, panel, false)
	color := rgb(153, 151, 148)
	if kind == noticeSuccess {
		color = rgb(91, 205, 74)
	}
	if kind == noticeWarning {
		color = rgb(255, 112, 99)
	}
	drawText(hdc, a.fontSmall, panel.Left+12, panel.Top, panel.Right-12, panel.Bottom, "> "+text, color, DT_LEFT|DT_VCENTER|DT_SINGLELINE|DT_END_ELLIPSIS)
}

func drawWindowButton(hdc uintptr, a *winApp, id controlID, panel RECT, text string) {
	a.mu.RLock()
	hovered, pressed := a.hovered == id, a.pressed == id
	a.mu.RUnlock()
	fill := a.brushPanel
	color := rgb(189, 74, 66)
	if hovered {
		fill, color = a.brushButtonError, rgb(255, 190, 182)
	}
	pFillRect.Call(hdc, uintptr(unsafe.Pointer(&panel)), fill)
	drawTechOutline(hdc, a, panel, hovered || pressed)
	offset := int32(0)
	if pressed {
		offset = 1
	}
	drawText(hdc, a.fontBody, panel.Left, panel.Top+offset-1, panel.Right, panel.Bottom+offset, text, color, DT_CENTER|DT_VCENTER|DT_SINGLELINE)
}

func drawSectionLabel(hdc uintptr, a *winApp, x, y int32, text string) {
	drawRule(hdc, a, x, y+18, 516, false)
	drawText(hdc, a.fontSmall, x, y, 360, y+20, text, rgb(242, 89, 76), DT_LEFT|DT_VCENTER|DT_SINGLELINE)
}

func drawRule(hdc uintptr, a *winApp, l, y, r int32, bright bool) {
	brush := a.brushLineDim
	if bright {
		brush = a.brushLine
	}
	pFillRect.Call(hdc, uintptr(unsafe.Pointer(&RECT{l, y, r, y + 1})), brush)
	pFillRect.Call(hdc, uintptr(unsafe.Pointer(&RECT{l, y - 2, l + 48, y + 3})), brush)
}

func drawTechFrame(hdc uintptr, a *winApp, rc RECT, active bool) {
	pFillRect.Call(hdc, uintptr(unsafe.Pointer(&rc)), a.brushPanel)
	drawTechOutline(hdc, a, rc, active)
}

func drawTechOutline(hdc uintptr, a *winApp, rc RECT, active bool) {
	brush := a.brushLineDim
	if active {
		brush = a.brushLine
	}
	pFillRect.Call(hdc, uintptr(unsafe.Pointer(&RECT{rc.Left, rc.Top, rc.Right, rc.Top + 1})), brush)
	pFillRect.Call(hdc, uintptr(unsafe.Pointer(&RECT{rc.Left, rc.Bottom - 1, rc.Right, rc.Bottom})), brush)
	pFillRect.Call(hdc, uintptr(unsafe.Pointer(&RECT{rc.Left, rc.Top, rc.Left + 1, rc.Bottom})), brush)
	pFillRect.Call(hdc, uintptr(unsafe.Pointer(&RECT{rc.Right - 1, rc.Top, rc.Right, rc.Bottom})), brush)
	pFillRect.Call(hdc, uintptr(unsafe.Pointer(&RECT{rc.Left, rc.Top, rc.Left + 16, rc.Top + 3})), brush)
	pFillRect.Call(hdc, uintptr(unsafe.Pointer(&RECT{rc.Right - 16, rc.Bottom - 3, rc.Right, rc.Bottom})), brush)
}

func drawTransmitSignal(hdc uintptr, a *winApp, rc RECT) {
	for i := int32(0); i < 3; i++ {
		inset := 5 + i*4
		pFillRect.Call(hdc, uintptr(unsafe.Pointer(&RECT{rc.Left + inset, rc.Top + 4, rc.Right - inset, rc.Top + 5})), a.brushLine)
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
func drawActionButton(hdc uintptr, a *winApp, id controlID, panel RECT, text string, disabled bool) {
	a.mu.RLock()
	hovered, pressed := a.hovered == id, a.pressed == id
	a.mu.RUnlock()
	fill := a.brushButton
	color := rgb(255, 82, 67)
	if hovered && !disabled {
		fill, color = a.brushButtonError, rgb(255, 160, 148)
	}
	if disabled {
		fill, color = a.brushPanel, rgb(113, 82, 79)
	}
	pFillRect.Call(hdc, uintptr(unsafe.Pointer(&panel)), fill)
	drawTechOutline(hdc, a, panel, hovered || pressed)
	offset := int32(0)
	if pressed && !disabled {
		offset = 2
	}
	drawText(hdc, a.fontBody, panel.Left, panel.Top+offset, panel.Right, panel.Bottom+offset, text, color, DT_CENTER|DT_VCENTER|DT_SINGLELINE)
}

func (a *winApp) controlAt(x, y int32) controlID {
	a.mu.RLock()
	paired, connected, pairing := a.paired, a.connected, a.pairing
	a.mu.RUnlock()
	if inRect(x, y, 490, 6, 520, 28) {
		return controlClose
	}
	if !paired {
		switch {
		case inRect(x, y, 315, 412, 455, 458):
			return controlChangeKey
		case !pairing && inRect(x, y, 85, 486, 455, 536):
			return controlPair
		}
	} else {
		switch {
		case inRect(x, y, 315, 376, 455, 422):
			return controlChangeKey
		case connected && inRect(x, y, 85, 452, 263, 506):
			return controlMinimize
		case !connected && inRect(x, y, 85, 452, 263, 506):
			return controlReconnect
		case inRect(x, y, 277, 452, 455, 506):
			return controlUnpair
		}
	}
	return controlNone
}

func (a *winApp) pressControl(x, y int32) {
	id := a.controlAt(x, y)
	if id == controlNone {
		return
	}
	a.mu.Lock()
	a.pressed = id
	a.mu.Unlock()
	pSetCapture.Call(a.hwnd)
	a.invalidateControl(id)
}

func (a *winApp) releaseControl(x, y int32) {
	a.mu.Lock()
	pressed := a.pressed
	a.pressed = controlNone
	a.mu.Unlock()
	pReleaseCapture.Call()
	a.invalidateControl(pressed)
	if pressed == controlNone || pressed != a.controlAt(x, y) {
		return
	}
	a.activate(pressed)
}

func (a *winApp) hoverControl(x, y int32) {
	tme := TRACKMOUSEEVENT{CbSize: uint32(unsafe.Sizeof(TRACKMOUSEEVENT{})), DwFlags: TME_LEAVE, HWndTrack: a.hwnd}
	pTrackMouseEvent.Call(uintptr(unsafe.Pointer(&tme)))
	a.setHovered(a.controlAt(x, y))
}

func (a *winApp) setHovered(id controlID) {
	a.mu.Lock()
	if a.hovered == id {
		a.mu.Unlock()
		return
	}
	previous := a.hovered
	a.hovered = id
	a.mu.Unlock()
	a.invalidateControl(previous)
	a.invalidateControl(id)
}

func (a *winApp) activate(id controlID) {
	switch id {
	case controlPair:
		go a.pairFromEdit()
	case controlChangeKey:
		a.beginKeyCapture()
	case controlMinimize:
		pShowWindow.Call(a.hwnd, SW_HIDE)
	case controlClose:
		pShowWindow.Call(a.hwnd, SW_HIDE)
	case controlReconnect:
		go a.reconnect()
	case controlUnpair:
		a.unpair()
	}
}
func inRect(x, y, l, t, r, b int32) bool { return x >= l && x <= r && y >= t && y <= b }

func (a *winApp) invalidateControl(id controlID) {
	if id == controlNone || a.hwnd == 0 {
		return
	}
	rc := RECT{26, 250, 514, 540}
	switch id {
	case controlClose:
		rc = RECT{490, 6, 520, 28}
	case controlChangeKey:
		rc = RECT{80, 340, 460, 465}
	case controlPair:
		rc = RECT{80, 480, 460, 540}
	case controlMinimize, controlReconnect, controlUnpair:
		rc = RECT{80, 445, 460, 512}
	}
	pInvalidateRect.Call(a.hwnd, uintptr(unsafe.Pointer(&rc)), 0)
}

func (a *winApp) hitTest(lParam uintptr) uintptr {
	pt := POINT{X: int32(int16(lParam & 0xffff)), Y: int32(int16((lParam >> 16) & 0xffff))}
	pScreenToClient.Call(a.hwnd, uintptr(unsafe.Pointer(&pt)))
	if a.controlAt(pt.X, pt.Y) != controlNone {
		return HTCLIENT
	}
	a.mu.RLock()
	paired := a.paired
	a.mu.RUnlock()
	if !paired && inRect(pt.X, pt.Y, 85, 328, 455, 366) {
		return HTCLIENT // Pair Code edit remains a normal native input control.
	}
	if !paired && inRect(pt.X, pt.Y, 85, 412, 300, 458) {
		return HTCLIENT // Radio Key field is reserved while pairing.
	}
	if paired && inRect(pt.X, pt.Y, 85, 376, 300, 422) {
		return HTCLIENT // Radio Key field is reserved while connected.
	}
	// Everything else is intentionally empty LCR background and can drag the
	// frameless utility without stealing any interactive control click.
	return HTCAPTION
}

func (a *winApp) pairFromEdit() {
	a.mu.Lock()
	if a.pairing {
		a.mu.Unlock()
		return
	}
	a.pairing = true
	a.pairError = ""
	a.notice = "CONNECTING TO COMMAND..."
	a.noticeKind = noticeInfo
	a.mu.Unlock()
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
	a.showWindow()
}

func (a *winApp) finishPairError(msg string) {
	a.mu.Lock()
	a.pairing = false
	a.pairError = msg
	a.notice = msg
	a.noticeKind = noticeWarning
	a.connected = false
	a.transmitting = false
	a.mu.Unlock()
	a.postState()
}

func (a *winApp) setRadioKey(k string) {
	a.mu.Lock()
	a.cfg.RadioKey = k
	a.cfg.RadioKeyKind = keyKindMouse
	a.cfg.RadioKeyCode = 0
	cfg := a.cfg
	a.mu.Unlock()
	_ = saveConfig(cfg)
	a.postState()
}

func (a *winApp) beginKeyCapture() {
	a.mu.Lock()
	if a.capturing {
		a.mu.Unlock()
		return
	}
	a.capturing = true
	a.notice = "PRESS ANY KEY OR MOUSE BUTTON"
	a.noticeKind = noticeInfo
	a.mu.Unlock()
	a.postState()
}

func (a *winApp) keyCaptureWorker() {
	for event := range a.keyEvents {
		a.mu.RLock()
		capturing := a.capturing
		a.mu.RUnlock()
		if !capturing {
			continue
		}
		if event.kind == keyKindKeyboard && event.vk == VK_ESCAPE {
			a.mu.Lock()
			a.capturing = false
			a.notice = "RADIO KEY UNCHANGED"
			a.noticeKind = noticeInfo
			a.mu.Unlock()
			a.postState()
			continue
		}
		name := event.name
		if event.kind == keyKindKeyboard {
			name = keyboardKeyName(event.vk, event.scanCode, event.flags)
		}
		if name == "" {
			continue
		}
		a.mu.Lock()
		a.cfg.RadioKey = name
		a.cfg.RadioKeyKind = event.kind
		a.cfg.RadioKeyCode = event.vk
		a.capturing = false
		a.notice = "RADIO KEY SET: " + name
		a.noticeKind = noticeSuccess
		cfg := a.cfg
		a.mu.Unlock()
		_ = saveConfig(cfg)
		a.postState()
	}
}

func keyboardKeyName(vk, scanCode, flags uint32) string {
	if name := friendlyVirtualKey(vk); name != "" {
		return name
	}
	lparam := scanCode << 16
	if flags&1 != 0 {
		lparam |= 1 << 24
	}
	buf := make([]uint16, 64)
	n, _, _ := pGetKeyNameText.Call(uintptr(lparam), uintptr(unsafe.Pointer(&buf[0])), uintptr(len(buf)))
	if n > 0 {
		return syscall.UTF16ToString(buf[:n])
	}
	return ""
}

func (a *winApp) reconnect() {
	a.setNotice("RECONNECTING TO COMMAND...", noticeInfo)
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	err := a.api.Probe(ctx)
	cancel()
	a.setConnected(err == nil)
}

func (a *winApp) unpair() {
	a.stopHeartbeat()
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	_ = a.api.SetState(ctx, false)
	cancel()
	a.mu.Lock()
	a.paired = false
	a.pairing = false
	a.pairError = ""
	a.notice = "ENTER PAIR CODE"
	a.noticeKind = noticeInfo
	a.connected = false
	a.transmitting = false
	a.cfg.DeviceToken = ""
	a.cfg.GuildID = ""
	a.cfg.UserID = ""
	cfg := a.cfg
	a.mu.Unlock()
	a.api.SetToken("")
	_ = saveConfig(cfg)
	pShowWindow.Call(a.edit, SW_SHOW)
	a.showWindow()
	a.postState()
}

func (a *winApp) installInputHooks() {
	mouseCB := syscall.NewCallback(mouseHookProc)
	keyboardCB := syscall.NewCallback(keyboardHookProc)
	hinst, _, _ := pGetModuleHandle.Call(0)
	a.mouseHook, _, _ = pSetWindowsHookEx.Call(WH_MOUSE_LL, mouseCB, hinst, 0)
	a.keyboardHook, _, _ = pSetWindowsHookEx.Call(WH_KEYBOARD_LL, keyboardCB, hinst, 0)
}
func mouseHookProc(nCode int, wParam, lParam uintptr) uintptr {
	a := currentApp
	if nCode == HC_ACTION && a != nil && lParam != 0 {
		s := (*MSLLHOOKSTRUCT)(unsafe.Pointer(lParam))
		a.mu.RLock()
		key := a.cfg.RadioKey
		kind := a.cfg.RadioKeyKind
		capturing := a.capturing
		a.mu.RUnlock()
		button, down := mouseButton(uint32(wParam), uint16((s.MouseData>>16)&0xffff))
		if capturing && down {
			if name := mouseKeyName(button); name != "" {
				select {
				case a.keyEvents <- keyCaptureEvent{kind: keyKindMouse, name: name}:
				default:
				}
			}
		} else if kind == keyKindMouse && mouseKeyName(button) == key {
			if down {
				select {
				case a.radioEvents <- true:
				default:
				}
			}
			if !down {
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

func mouseButton(message uint32, xbutton uint16) (uint16, bool) {
	switch message {
	case WM_LBUTTONDOWN:
		return 3, true
	case WM_LBUTTONUP:
		return 3, false
	case 0x0204:
		return 4, true
	case 0x0205:
		return 4, false
	case 0x0207:
		return 5, true
	case 0x0208:
		return 5, false
	case WM_XBUTTONDOWN:
		return xbutton, true
	case WM_XBUTTONUP:
		return xbutton, false
	}
	return 0, false
}

func keyboardHookProc(nCode int, wParam, lParam uintptr) uintptr {
	a := currentApp
	if nCode == HC_ACTION && a != nil && lParam != 0 {
		s := (*KBDLLHOOKSTRUCT)(unsafe.Pointer(lParam))
		message := uint32(wParam)
		down := message == WM_KEYDOWN || message == WM_SYSKEYDOWN
		up := message == WM_KEYUP || message == WM_SYSKEYUP
		a.mu.RLock()
		capturing := a.capturing
		kind, keyCode := a.cfg.RadioKeyKind, a.cfg.RadioKeyCode
		a.mu.RUnlock()
		if capturing && down {
			select {
			case a.keyEvents <- keyCaptureEvent{kind: keyKindKeyboard, vk: s.VkCode, scanCode: s.ScanCode, flags: s.Flags}:
			default:
			}
		} else if kind == keyKindKeyboard && s.VkCode == keyCode {
			if down {
				select {
				case a.radioEvents <- true:
				default:
				}
			}
			if up {
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
		current := a.transmitting
		a.mu.RUnlock()
		if !paired {
			continue
		}
		target := down
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
	wasConnected := a.connected
	a.connected = v
	if !v {
		a.transmitting = false
	}
	sound := uintptr(0)
	if v && !wasConnected {
		a.notice = "LINK ESTABLISHED"
		a.noticeKind = noticeSuccess
		sound = MB_ICONASTERISK
	}
	if !v && wasConnected {
		a.notice = "CONNECTION LOST"
		a.noticeKind = noticeWarning
		sound = MB_ICONEXCLAMATION
	}
	a.mu.Unlock()
	a.postState()
	if sound != 0 {
		pMessageBeep.Call(sound)
	}
}

func (a *winApp) setNotice(text string, kind noticeKind) {
	a.mu.Lock()
	a.notice = text
	a.noticeKind = kind
	a.mu.Unlock()
	a.postState()
}
func (a *winApp) setState(conn, tx bool) {
	a.mu.Lock()
	wasConnected := a.connected
	a.connected = conn
	a.transmitting = tx
	sound := uintptr(0)
	if conn && !wasConnected {
		a.notice = "LINK ESTABLISHED"
		a.noticeKind = noticeSuccess
		sound = MB_ICONASTERISK
	}
	if !conn && wasConnected {
		a.notice = "CONNECTION LOST"
		a.noticeKind = noticeWarning
		sound = MB_ICONEXCLAMATION
	}
	a.mu.Unlock()
	a.postState()
	if sound != 0 {
		pMessageBeep.Call(sound)
	}
}
func (a *winApp) postState() {
	if a.hwnd != 0 {
		pPostMessage.Call(a.hwnd, WM_STATE, 0, 0)
	}
}

func (a *winApp) loadIcons() {
	dir := filepath.Join(os.TempDir(), "LCR-assets")
	_ = os.MkdirAll(dir, 0o700)
	load := func(name string) uintptr {
		b, _ := fs.ReadFile(assetFS, "assets/"+name)
		p := filepath.Join(dir, name)
		_ = os.WriteFile(p, b, 0o600)
		h, _, _ := pLoadImage.Call(0, uintptr(unsafe.Pointer(ptr(p))), IMAGE_ICON, 0, 0, LR_LOADFROMFILE|LR_DEFAULTSIZE)
		return h
	}
	a.iconGreen = load("green.ico")
	a.iconRed = load("red.ico")
	a.iconGray = load("gray.ico")
}

func (a *winApp) loadBanner() {
	input := GdiplusStartupInput{GdiplusVersion: 1}
	if status, _, _ := pGdiplusStartup.Call(uintptr(unsafe.Pointer(&a.gdiplusToken)), uintptr(unsafe.Pointer(&input)), 0); status != 0 {
		return
	}
	dir := filepath.Join(os.TempDir(), "LCR-assets")
	_ = os.MkdirAll(dir, 0o700)
	b, err := fs.ReadFile(assetFS, "assets/lcr-banner.png")
	if err != nil {
		return
	}
	path := filepath.Join(dir, "lcr-banner.png")
	if os.WriteFile(path, b, 0o600) != nil {
		return
	}
	if status, _, _ := pGdipLoadImageFromFile.Call(uintptr(unsafe.Pointer(ptr(path))), uintptr(unsafe.Pointer(&a.bannerImage))); status != 0 {
		a.bannerImage = 0
	}
}

func (a *winApp) drawBanner(hdc uintptr) {
	if a.bannerImage == 0 {
		return
	}
	var graphics uintptr
	if status, _, _ := pGdipCreateFromHDC.Call(hdc, uintptr(unsafe.Pointer(&graphics))); status != 0 || graphics == 0 {
		return
	}
	defer pGdipDeleteGraphics.Call(graphics)
	// The banner owns its header rectangle; controls begin below it.
	pGdipDrawImageRectI.Call(graphics, a.bannerImage, 8, 32, 524, 129)
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
	tip := "LCR — disconnected"
	if conn {
		tip = "LCR — standby"
	}
	if tx {
		tip = "LCR — transmitting"
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
	a.centerWindow()
	pShowWindow.Call(a.hwnd, SW_SHOWNORMAL)
	pSetForegroundWindow.Call(a.hwnd)
	pInvalidateRect.Call(a.hwnd, 0, 0)
	pMessageBeep.Call(MB_OK)
}

func (a *winApp) centerWindow() {
	monitor, _, _ := pMonitorFromWindow.Call(a.hwnd, MONITOR_DEFAULTTONEAREST)
	if monitor == 0 {
		return
	}
	mi := MONITORINFO{CbSize: uint32(unsafe.Sizeof(MONITORINFO{}))}
	if ok, _, _ := pGetMonitorInfo.Call(monitor, uintptr(unsafe.Pointer(&mi))); ok == 0 {
		return
	}
	var window RECT
	if ok, _, _ := pGetWindowRect.Call(a.hwnd, uintptr(unsafe.Pointer(&window))); ok == 0 {
		return
	}
	width, height := window.Right-window.Left, window.Bottom-window.Top
	x := mi.RcWork.Left + (mi.RcWork.Right-mi.RcWork.Left-width)/2
	y := mi.RcWork.Top + (mi.RcWork.Bottom-mi.RcWork.Top-height)/2
	pSetWindowPos.Call(a.hwnd, 0, uintptr(x), uintptr(y), 0, 0, SWP_NOSIZE|SWP_NOZORDER)
}

func (a *winApp) showTrayMenu() {
	menu, _, _ := pCreatePopupMenu.Call()
	if menu == 0 {
		return
	}
	defer pDestroyMenu.Call(menu)
	appendMenu(menu, MF_STRING, cmdOpen, "Open LCR")
	appendMenu(menu, MF_SEPARATOR, 0, "")
	a.mu.RLock()
	cfg := a.cfg
	a.mu.RUnlock()
	appendMenu(menu, MF_STRING, cmdChangeKey, "Radio Key: "+cfg.RadioKey)
	appendMenu(menu, MF_SEPARATOR, 0, "")
	appendMenu(menu, MF_STRING, cmdReconnect, "Reconnect")
	appendMenu(menu, MF_STRING, cmdUnpair, "Unpair")
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
	case cmdOpen:
		a.showWindow()
	case cmdChangeKey:
		a.showWindow()
		a.beginKeyCapture()
	case cmdReconnect:
		go a.reconnect()
	case cmdUnpair:
		a.unpair()
	case cmdUninstall:
		r, _, _ := pMessageBox.Call(a.hwnd, uintptr(unsafe.Pointer(ptr("Uninstall LCR?"))), uintptr(unsafe.Pointer(ptr(appName))), MB_YESNO|MB_ICONQUESTION)
		if r == IDYES {
			a.quitting = true
			a.deleteTray()
			_ = performUninstall()
			pPostQuitMessage.Call(0)
		}
	case cmdExit:
		a.mu.Lock()
		a.quitting = true
		a.mu.Unlock()
		pPostQuitMessage.Call(0)
	}
}
