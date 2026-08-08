//go:build windows

package main

import (
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"syscall"
	"time"
)

func installPath() string {
	base := os.Getenv("LOCALAPPDATA")
	if base == "" {
		base, _ = os.UserHomeDir()
	}
	return filepath.Join(base, "Programs", "LLB Command Radio", "LLBCommandRadio.exe")
}

// ensureInstalled returns true when the current process is already the installed copy.
// On first launch it copies itself to LocalAppData, registers startup + uninstall,
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
	runKey := `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`
	uninstallKey := `HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\LLBCommandRadio`
	if err := regAdd(runKey, "LLBCommandRadio", "REG_SZ", `"`+target+`" --startup`); err != nil {
		return fmt.Errorf("register startup: %w", err)
	}
	pairs := [][2]string{
		{"DisplayName", appName}, {"DisplayVersion", buildVersion}, {"Publisher", "Linh Lan Bang"},
		{"InstallLocation", filepath.Dir(target)}, {"UninstallString", `"` + target + `" --uninstall`},
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

func removeInstallRegistration() error {
	regDeleteValue(`HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, "LLBCommandRadio")
	regDelete(`HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\LLBCommandRadio`)
	return nil
}

func performUninstall() error {
	_ = removeInstallRegistration()
	// Preserve nothing: uninstall means helper + local pairing token/config are removed.
	_ = os.RemoveAll(filepath.Dir(configPath()))
	scheduleSelfDelete()
	return nil
}

func scheduleSelfDelete() {
	exe, _ := os.Executable()
	dir := filepath.Dir(exe)
	// cmd waits briefly so this process can exit, then removes the installed binary and directory.
	cmd := fmt.Sprintf(`ping 127.0.0.1 -n 3 >nul & del /F /Q "%s" >nul 2>&1 & rmdir /Q "%s" >nul 2>&1`, exe, dir)
	c := hiddenCommand("cmd.exe", "/C", cmd)
	_ = c.Start()
	time.Sleep(25 * time.Millisecond)
}
