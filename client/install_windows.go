//go:build windows

package main

import (
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"syscall"
	"time"
	"unsafe"
)

type guid struct {
	Data1 uint32
	Data2 uint16
	Data3 uint16
	Data4 [8]byte
}

var (
	ole32                 = syscall.NewLazyDLL("ole32.dll")
	shell32Shortcut       = syscall.NewLazyDLL("shell32.dll")
	pCoInitializeEx       = ole32.NewProc("CoInitializeEx")
	pCoUninitialize       = ole32.NewProc("CoUninitialize")
	pCoCreateInstance     = ole32.NewProc("CoCreateInstance")
	pCoTaskMemFree        = ole32.NewProc("CoTaskMemFree")
	pSHGetKnownFolderPath = shell32Shortcut.NewProc("SHGetKnownFolderPath")
	clsidShellLink        = guid{0x00021401, 0x0000, 0x0000, [8]byte{0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46}}
	iidShellLinkW         = guid{0x000214F9, 0x0000, 0x0000, [8]byte{0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46}}
	iidPersistFile        = guid{0x0000010B, 0x0000, 0x0000, [8]byte{0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46}}
	folderIDDesktop       = guid{0xB4BFCC3A, 0xDB2C, 0x424C, [8]byte{0xB0, 0x29, 0x7F, 0xE9, 0x9A, 0x87, 0xC6, 0x41}}
)

func installPath() string {
	base := os.Getenv("LOCALAPPDATA")
	if base == "" {
		base, _ = os.UserHomeDir()
	}
	return filepath.Join(base, "Programs", "LCR", "LCR.exe")
}

// ensureInstalled returns true when the current process is already the installed copy.
// On first launch it copies itself to LocalAppData, registers uninstall,
// relaunches the installed copy, and returns false so the downloaded bootstrap exits.
func ensureInstalled() (bool, error) {
	cur, err := os.Executable()
	if err != nil {
		return false, err
	}
	cur, _ = filepath.Abs(cur)
	target := installPath()
	targetAbs, _ := filepath.Abs(target)
	if strings.EqualFold(cur, targetAbs) {
		return true, nil
	}
	if err := os.MkdirAll(filepath.Dir(target), 0o755); err != nil {
		return false, err
	}
	if err := copyFile(cur, target); err != nil {
		return false, fmt.Errorf("install copy: %w", err)
	}
	if err := registerInstall(target); err != nil {
		return false, err
	}
	removeLegacyStartupRegistration()
	if err := createDesktopShortcut(target); err != nil {
		return false, err
	}
	if err := exec.Command(target).Start(); err != nil {
		return false, fmt.Errorf("launch installed helper: %w", err)
	}
	return false, nil
}

func copyFile(src, dst string) error {
	in, err := os.Open(src)
	if err != nil {
		return err
	}
	defer in.Close()
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

func hiddenCommand(name string, args ...string) *exec.Cmd {
	c := exec.Command(name, args...)
	// GUI helper must never flash a console window for reg.exe/cmd.exe helpers.
	c.SysProcAttr = &syscall.SysProcAttr{HideWindow: true}
	return c
}

func regAdd(key, name, typ, value string) error {
	args := []string{"add", key, "/v", name, "/t", typ, "/d", value, "/f"}
	return hiddenCommand("reg.exe", args...).Run()
}
func regDelete(key string) { _ = hiddenCommand("reg.exe", "delete", key, "/f").Run() }
func regDeleteValue(key, name string) {
	_ = hiddenCommand("reg.exe", "delete", key, "/v", name, "/f").Run()
}

func registerInstall(target string) error {
	uninstallKey := `HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\LCR`
	pairs := [][2]string{
		{"DisplayName", appName}, {"DisplayVersion", buildVersion}, {"Publisher", "Linh Lan Bang"},
		{"InstallLocation", filepath.Dir(target)}, {"UninstallString", `"` + target + `" --uninstall`},
		{"DisplayIcon", target + ",0"},
		{"NoModify", "1"}, {"NoRepair", "1"},
	}
	for _, p := range pairs {
		typ := "REG_SZ"
		if p[0] == "NoModify" || p[0] == "NoRepair" {
			typ = "REG_DWORD"
		}
		if err := regAdd(uninstallKey, p[0], typ, p[1]); err != nil {
			return err
		}
	}
	return nil
}

func removeLegacyStartupRegistration() {
	regDeleteValue(`HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, "LCR")
}

func removeInstallRegistration() error {
	removeLegacyStartupRegistration()
	regDelete(`HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\LCR`)
	return nil
}

func performUninstall() error {
	_ = removeInstallRegistration()
	if shortcut, err := desktopShortcutPath(); err == nil {
		_ = os.Remove(shortcut)
	}
	// Preserve nothing: uninstall means helper + local pairing token/config are removed.
	_ = os.RemoveAll(filepath.Dir(configPath()))
	_ = os.RemoveAll(filepath.Dir(legacyConfigPath()))
	return launchCleanup()
}

func launchCleanup() error {
	target, err := os.Executable()
	if err != nil {
		return err
	}
	cleanupDir, err := os.MkdirTemp("", "LCR-cleanup-")
	if err != nil {
		return err
	}
	cleanup := filepath.Join(cleanupDir, "LCR-cleanup.exe")
	if err := copyFile(target, cleanup); err != nil {
		_ = os.RemoveAll(cleanupDir)
		return err
	}
	cmd := exec.Command(cleanup, "--cleanup", "--target", target, "--dir", filepath.Dir(target))
	cmd.SysProcAttr = &syscall.SysProcAttr{HideWindow: true, CreationFlags: 0x00000008 | 0x00000200}
	if err := cmd.Start(); err != nil {
		_ = os.Remove(cleanup)
		_ = os.Remove(cleanupDir)
		return err
	}
	return nil
}

func performCleanup(args []string) error {
	target, dir := argValue(args, "--target"), argValue(args, "--dir")
	if target == "" || dir == "" || filepath.Dir(target) != dir {
		return fmt.Errorf("invalid cleanup target")
	}
	// The temporary cleanup copy is not named LCR.exe, so this only closes
	// installed helper instances before retrying deletion of the owned target.
	_ = hiddenCommand("taskkill.exe", "/F", "/IM", "LCR.exe").Run()
	deadline := time.Now().Add(12 * time.Second)
	for {
		err := os.Remove(target)
		if err == nil || os.IsNotExist(err) {
			_ = os.Remove(dir) // remove only an empty LCR install directory
			break
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("remove installed helper: %w", err)
		}
		time.Sleep(100 * time.Millisecond)
	}
	scheduleCleanupSelf()
	return nil
}

func argValue(args []string, name string) string {
	for i := 0; i+1 < len(args); i++ {
		if strings.EqualFold(args[i], name) {
			return args[i+1]
		}
	}
	return ""
}

func desktopShortcutPath() (string, error) {
	var desktop *uint16
	hr, _, _ := pSHGetKnownFolderPath.Call(
		uintptr(unsafe.Pointer(&folderIDDesktop)), 0, 0, uintptr(unsafe.Pointer(&desktop)),
	)
	if int32(hr) < 0 || desktop == nil {
		return "", fmt.Errorf("SHGetKnownFolderPath(FOLDERID_Desktop): %#x", hr)
	}
	defer pCoTaskMemFree.Call(uintptr(unsafe.Pointer(desktop)))
	return filepath.Join(utf16PointerString(desktop), "LCR.lnk"), nil
}

func utf16PointerString(value *uint16) string {
	if value == nil {
		return ""
	}
	chars := unsafe.Slice(value, 32768)
	for i, char := range chars {
		if char == 0 {
			return syscall.UTF16ToString(chars[:i])
		}
	}
	return syscall.UTF16ToString(chars)
}

func createDesktopShortcut(target string) error {
	shortcutPath, err := desktopShortcutPath()
	if err != nil {
		return err
	}
	shortcut, err := syscall.UTF16PtrFromString(shortcutPath)
	if err != nil {
		return err
	}
	targetPath, err := syscall.UTF16PtrFromString(target)
	if err != nil {
		return err
	}
	hr, _, _ := pCoInitializeEx.Call(0, 2) // COINIT_APARTMENTTHREADED
	if int32(hr) < 0 {
		return fmt.Errorf("CoInitializeEx: %#x", hr)
	}
	defer pCoUninitialize.Call()

	var link uintptr
	hr, _, _ = pCoCreateInstance.Call(
		uintptr(unsafe.Pointer(&clsidShellLink)), 0, 1, // CLSCTX_INPROC_SERVER
		uintptr(unsafe.Pointer(&iidShellLinkW)), uintptr(unsafe.Pointer(&link)),
	)
	if int32(hr) < 0 || link == 0 {
		return fmt.Errorf("CoCreateInstance(IShellLinkW): %#x", hr)
	}
	defer comRelease(link)
	if hr = comCall(link, 20, uintptr(unsafe.Pointer(targetPath))); int32(hr) < 0 { // IShellLinkW::SetPath
		return fmt.Errorf("IShellLinkW.SetPath: %#x", hr)
	}
	if hr = comCall(link, 17, uintptr(unsafe.Pointer(targetPath)), 0); int32(hr) < 0 { // IShellLinkW::SetIconLocation
		return fmt.Errorf("IShellLinkW.SetIconLocation: %#x", hr)
	}

	var persist uintptr
	if hr = comCall(link, 0, uintptr(unsafe.Pointer(&iidPersistFile)), uintptr(unsafe.Pointer(&persist))); int32(hr) < 0 || persist == 0 {
		return fmt.Errorf("IShellLinkW.QueryInterface(IPersistFile): %#x", hr)
	}
	defer comRelease(persist)
	if hr = comCall(persist, 6, uintptr(unsafe.Pointer(shortcut)), 1); int32(hr) < 0 { // IPersistFile::Save
		return fmt.Errorf("IPersistFile.Save: %#x", hr)
	}
	runtime.KeepAlive(targetPath)
	runtime.KeepAlive(shortcut)
	return nil
}

func comCall(obj uintptr, slot uintptr, args ...uintptr) uintptr {
	vtable := *(*uintptr)(unsafe.Pointer(obj))
	method := *(*uintptr)(unsafe.Pointer(vtable + slot*unsafe.Sizeof(uintptr(0))))
	callArgs := append([]uintptr{obj}, args...)
	r1, _, _ := syscall.SyscallN(method, callArgs...)
	return r1
}

func comRelease(obj uintptr) {
	if obj != 0 {
		_ = comCall(obj, 2)
	}
}

func scheduleCleanupSelf() {
	exe, _ := os.Executable()
	// This only removes the temporary cleanup copy after it exits; no second
	// executable is kept in the installed LCR directory.
	cmd := fmt.Sprintf(`ping 127.0.0.1 -n 3 >nul & del /F /Q "%s" >nul 2>&1 & rmdir /Q "%s" >nul 2>&1`, exe, filepath.Dir(exe))
	c := exec.Command("cmd.exe", "/C", cmd)
	// Detached cleanup survives when the uninstaller was launched from a
	// parent process/job that closes its children as the GUI exits.
	c.SysProcAttr = &syscall.SysProcAttr{HideWindow: true, CreationFlags: 0x00000008 | 0x00000200}
	_ = c.Start()
	time.Sleep(25 * time.Millisecond)
}
