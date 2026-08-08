//go:build windows

package main

import (
	"sync"
	"syscall"
	"unsafe"
)

const settingsClassName = "LCRSettingsWindow"

type settingsControl uint8

const (
	settingsNone settingsControl = iota
	settingsDefault
	settingsSave
	settingsClose
)

type settingsDialog struct {
	app              *winApp
	hwnd, edit       uintptr
	hovered, pressed settingsControl
	closed           bool
	message          string
}

var (
	settingsClassOnce sync.Once
	settingsClassErr  error
	activeSettings    *settingsDialog
)

func registerSettingsClass() error {
	settingsClassOnce.Do(func() {
		hinst, _, _ := pGetModuleHandle.Call(0)
		cursor, _, _ := pLoadCursor.Call(0, IDC_ARROW)
		class := ptr(settingsClassName)
		wc := WNDCLASSEX{CbSize: uint32(unsafe.Sizeof(WNDCLASSEX{})), LpfnWndProc: syscall.NewCallback(settingsWndProc), HInstance: hinst, HCursor: cursor, LpszClassName: class}
		if r, _, e := pRegisterClassEx.Call(uintptr(unsafe.Pointer(&wc))); r == 0 {
			settingsClassErr = e
		}
	})
	return settingsClassErr
}

func (a *winApp) openSettings() {
	if err := registerSettingsClass(); err != nil {
		a.setNotice("SETTINGS UNAVAILABLE", noticeWarning)
		return
	}
	a.mu.RLock()
	serverURL := a.cfg.ServerURL
	a.mu.RUnlock()
	d := &settingsDialog{app: a}
	activeSettings = d
	hinst, _, _ := pGetModuleHandle.Call(0)
	hwnd, _, _ := pCreateWindowEx.Call(WS_EX_TOOLWINDOW, uintptr(unsafe.Pointer(ptr(settingsClassName))), uintptr(unsafe.Pointer(ptr("LCR Settings"))), uintptr(WS_POPUP|WS_SYSMENU), 0, 0, 460, 270, a.hwnd, 0, hinst, 0)
	if hwnd == 0 {
		activeSettings = nil
		a.setNotice("SETTINGS UNAVAILABLE", noticeWarning)
		return
	}
	d.hwnd = hwnd
	d.edit, _, _ = pCreateWindowEx.Call(0, uintptr(unsafe.Pointer(ptr("EDIT"))), uintptr(unsafe.Pointer(ptr(serverURL))), uintptr(WS_CHILD|WS_VISIBLE|ES_AUTOHSCROLL), 36, 102, 388, 30, hwnd, 0, hinst, 0)
	pSendMessage.Call(d.edit, WM_SETFONT, a.fontBody, 1)
	d.centerOverOwner()
	a.mu.Lock()
	a.modal = true
	paired := a.paired
	a.mu.Unlock()
	if !paired {
		pShowWindow.Call(a.edit, SW_HIDE)
	}
	pInvalidateRect.Call(a.hwnd, 0, 0)
	pEnableWindow.Call(a.hwnd, 0)
	pShowWindow.Call(hwnd, SW_SHOW)
	pSetForegroundWindow.Call(hwnd)

	quit := false
	for !d.closed {
		var msg MSG
		r, _, _ := pGetMessage.Call(uintptr(unsafe.Pointer(&msg)), 0, 0, 0)
		if int32(r) <= 0 {
			quit = r == 0
			break
		}
		if msg.Message == WM_KEYDOWN {
			if msg.WParam == VK_ESCAPE {
				d.dismiss()
				continue
			}
			if msg.WParam == VK_RETURN {
				d.save()
				continue
			}
		}
		pTranslateMessage.Call(uintptr(unsafe.Pointer(&msg)))
		pDispatchMessage.Call(uintptr(unsafe.Pointer(&msg)))
	}
	pEnableWindow.Call(a.hwnd, 1)
	a.mu.Lock()
	a.modal = false
	paired = a.paired
	a.mu.Unlock()
	if !paired {
		pShowWindow.Call(a.edit, SW_SHOW)
	}
	pInvalidateRect.Call(a.hwnd, 0, 0)
	pSetForegroundWindow.Call(a.hwnd)
	activeSettings = nil
	if quit {
		pPostQuitMessage.Call(0)
	}
}

func (d *settingsDialog) centerOverOwner() {
	var owner RECT
	pGetWindowRect.Call(d.app.hwnd, uintptr(unsafe.Pointer(&owner)))
	pSetWindowPos.Call(d.hwnd, 0, uintptr(owner.Left+(owner.Right-owner.Left-460)/2), uintptr(owner.Top+(owner.Bottom-owner.Top-270)/2), 0, 0, SWP_NOSIZE|SWP_NOZORDER)
}

func settingsWndProc(hwnd uintptr, msg uint32, wParam, lParam uintptr) uintptr {
	d := activeSettings
	if d == nil {
		r, _, _ := pDefWindowProc.Call(hwnd, uintptr(msg), wParam, lParam)
		return r
	}
	switch msg {
	case WM_ERASEBKGND:
		return 1
	case WM_NCHITTEST:
		return d.hitTest(lParam)
	case WM_PAINT:
		d.paint()
		return 0
	case WM_CLOSE:
		d.dismiss()
		return 0
	case WM_DESTROY:
		d.closed = true
		return 0
	case WM_LBUTTONDOWN:
		d.press(int32(int16(lParam&0xffff)), int32(int16((lParam>>16)&0xffff)))
		return 0
	case WM_LBUTTONUP:
		d.release(int32(int16(lParam&0xffff)), int32(int16((lParam>>16)&0xffff)))
		return 0
	case WM_MOUSEMOVE:
		tme := TRACKMOUSEEVENT{CbSize: uint32(unsafe.Sizeof(TRACKMOUSEEVENT{})), DwFlags: TME_LEAVE, HWndTrack: d.hwnd}
		pTrackMouseEvent.Call(uintptr(unsafe.Pointer(&tme)))
		d.setHovered(d.controlAt(int32(int16(lParam&0xffff)), int32(int16((lParam>>16)&0xffff))))
		return 0
	case WM_MOUSELEAVE:
		d.setHovered(settingsNone)
		return 0
	case WM_CTLCOLOREDIT:
		pSetTextColor.Call(wParam, rgb(255, 82, 67))
		pSetBkColor.Call(wParam, rgb(22, 17, 18))
		return d.app.brushEdit
	}
	r, _, _ := pDefWindowProc.Call(hwnd, uintptr(msg), wParam, lParam)
	return r
}

func (d *settingsDialog) paint() {
	var ps PAINTSTRUCT
	paintDC, _, _ := pBeginPaint.Call(d.hwnd, uintptr(unsafe.Pointer(&ps)))
	defer pEndPaint.Call(d.hwnd, uintptr(unsafe.Pointer(&ps)))
	var client RECT
	pGetClientRect.Call(d.hwnd, uintptr(unsafe.Pointer(&client)))
	memDC, _, _ := pCreateCompatibleDC.Call(paintDC)
	if memDC == 0 {
		return
	}
	defer pDeleteDC.Call(memDC)
	bitmap, _, _ := pCreateCompatibleBitmap.Call(paintDC, uintptr(client.Right-client.Left), uintptr(client.Bottom-client.Top))
	if bitmap == 0 {
		return
	}
	oldBitmap, _, _ := pSelectObject.Call(memDC, bitmap)
	defer func() {
		pBitBlt.Call(paintDC, 0, 0, uintptr(client.Right-client.Left), uintptr(client.Bottom-client.Top), memDC, 0, 0, SRCCOPY)
		pSelectObject.Call(memDC, oldBitmap)
		pDeleteObject.Call(bitmap)
	}()
	hdc := memDC
	bg := RECT{0, 0, 460, 270}
	pFillRect.Call(hdc, uintptr(unsafe.Pointer(&bg)), d.app.brushBG)
	pSetBkMode.Call(hdc, TRANSPARENT)
	drawTechOutline(hdc, d.app, RECT{8, 8, 452, 262}, true)
	drawText(hdc, d.app.fontTitle, 28, 24, 370, 58, "LCR SETTINGS", rgb(255, 82, 67), DT_LEFT|DT_VCENTER|DT_SINGLELINE)
	d.drawWindowButton(hdc, settingsClose, RECT{418, 18, 446, 40}, "×")
	drawRule(hdc, d.app, 28, 70, 430, false)
	drawText(hdc, d.app.fontSmall, 34, 78, 330, 98, "SERVER ADDRESS", rgb(185, 145, 141), DT_LEFT|DT_VCENTER|DT_SINGLELINE)
	drawTechFrame(hdc, d.app, RECT{30, 96, 430, 138}, true)
	drawText(hdc, d.app.fontSmall, 34, 146, 426, 168, "> SERVER ADDRESS CONTROLS RADIO LINK", rgb(153, 151, 148), DT_LEFT|DT_VCENTER|DT_SINGLELINE)
	if d.message != "" {
		drawText(hdc, d.app.fontSmall, 34, 168, 426, 188, "> "+d.message, rgb(255, 112, 99), DT_LEFT|DT_VCENTER|DT_SINGLELINE|DT_END_ELLIPSIS)
	}
	d.drawActionButton(hdc, settingsDefault, RECT{34, 206, 218, 248}, "USE DEFAULT")
	d.drawActionButton(hdc, settingsSave, RECT{242, 206, 426, 248}, "SAVE")
}

func (d *settingsDialog) drawWindowButton(hdc uintptr, id settingsControl, panel RECT, text string) {
	fill, color := d.app.brushPanel, rgb(189, 74, 66)
	if d.hovered == id {
		fill, color = d.app.brushButtonError, rgb(255, 190, 182)
	}
	pFillRect.Call(hdc, uintptr(unsafe.Pointer(&panel)), fill)
	drawTechOutline(hdc, d.app, panel, d.hovered == id || d.pressed == id)
	drawText(hdc, d.app.fontBody, panel.Left, panel.Top-1, panel.Right, panel.Bottom, text, color, DT_CENTER|DT_VCENTER|DT_SINGLELINE)
}

func (d *settingsDialog) drawActionButton(hdc uintptr, id settingsControl, panel RECT, text string) {
	fill, color := d.app.brushButton, rgb(255, 82, 67)
	if d.hovered == id {
		fill, color = d.app.brushButtonError, rgb(255, 160, 148)
	}
	pFillRect.Call(hdc, uintptr(unsafe.Pointer(&panel)), fill)
	drawTechOutline(hdc, d.app, panel, d.hovered == id || d.pressed == id)
	offset := int32(0)
	if d.pressed == id {
		offset = 2
	}
	drawText(hdc, d.app.fontBody, panel.Left, panel.Top+offset, panel.Right, panel.Bottom+offset, text, color, DT_CENTER|DT_VCENTER|DT_SINGLELINE)
}

func (d *settingsDialog) controlAt(x, y int32) settingsControl {
	if inRect(x, y, 418, 18, 446, 40) {
		return settingsClose
	}
	if inRect(x, y, 34, 206, 218, 248) {
		return settingsDefault
	}
	if inRect(x, y, 242, 206, 426, 248) {
		return settingsSave
	}
	return settingsNone
}

func (d *settingsDialog) hitTest(lParam uintptr) uintptr {
	pt := POINT{X: int32(int16(lParam & 0xffff)), Y: int32(int16((lParam >> 16) & 0xffff))}
	pScreenToClient.Call(d.hwnd, uintptr(unsafe.Pointer(&pt)))
	if d.controlAt(pt.X, pt.Y) != settingsNone || inRect(pt.X, pt.Y, 34, 100, 426, 134) {
		return HTCLIENT
	}
	return HTCAPTION
}

func (d *settingsDialog) press(x, y int32) {
	if id := d.controlAt(x, y); id != settingsNone {
		d.pressed = id
		pSetCapture.Call(d.hwnd)
		d.invalidateControl(id)
	}
}
func (d *settingsDialog) release(x, y int32) {
	pressed := d.pressed
	d.pressed = settingsNone
	pReleaseCapture.Call()
	d.invalidateControl(pressed)
	if pressed != settingsNone && pressed == d.controlAt(x, y) {
		d.activate(pressed)
	}
}
func (d *settingsDialog) setHovered(id settingsControl) {
	if d.hovered != id {
		previous := d.hovered
		d.hovered = id
		d.invalidateControl(previous)
		d.invalidateControl(id)
	}
}
func (d *settingsDialog) invalidateControl(id settingsControl) {
	if id == settingsNone {
		return
	}
	rc := RECT{34, 206, 426, 248}
	switch id {
	case settingsClose:
		rc = RECT{418, 18, 446, 40}
	case settingsDefault:
		rc = RECT{34, 206, 218, 248}
	case settingsSave:
		rc = RECT{242, 206, 426, 248}
	}
	pInvalidateRect.Call(d.hwnd, uintptr(unsafe.Pointer(&rc)), 0)
}
func (d *settingsDialog) invalidateMessage() {
	rc := RECT{28, 146, 430, 190}
	pInvalidateRect.Call(d.hwnd, uintptr(unsafe.Pointer(&rc)), 0)
}
func (d *settingsDialog) activate(id settingsControl) {
	switch id {
	case settingsClose:
		d.dismiss()
	case settingsDefault:
		pSetWindowText.Call(d.edit, uintptr(unsafe.Pointer(ptr(defaultServerURL))))
	case settingsSave:
		d.save()
	}
}

func (d *settingsDialog) save() {
	buf := make([]uint16, 2048)
	n, _, _ := pGetWindowText.Call(d.edit, uintptr(unsafe.Pointer(&buf[0])), uintptr(len(buf)))
	endpoint, ok := normalizeServerURL(syscall.UTF16ToString(buf[:n]))
	if !ok {
		d.message = "ENTER A VALID HTTPS OR LOCAL ADDRESS"
		d.invalidateMessage()
		return
	}
	if err := d.app.updateServerURL(endpoint); err != nil {
		d.message = "SERVER ADDRESS NOT SAVED"
		d.invalidateMessage()
		return
	}
	d.dismiss()
}

func (d *settingsDialog) dismiss() {
	if !d.closed && d.hwnd != 0 {
		pDestroyWindow.Call(d.hwnd)
	}
}
