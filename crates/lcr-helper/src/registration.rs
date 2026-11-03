use crate::distribution::{
    install_dir, install_path, uninstall_command, CURRENT_VERSION, UNINSTALL_REGISTRY_SUBKEY,
};
use std::{
    ffi::OsStr,
    io,
    mem::size_of,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    ptr::{null, null_mut},
};

type HKey = isize;
type LStatus = i32;

const HKEY_CURRENT_USER: HKey = 0x8000_0001u32 as i32 as isize;
const REG_OPTION_NON_VOLATILE: u32 = 0;
const KEY_SET_VALUE: u32 = 0x0002;
const REG_SZ: u32 = 1;
const REG_DWORD: u32 = 4;
const ERROR_FILE_NOT_FOUND: LStatus = 2;
const ERROR_PATH_NOT_FOUND: LStatus = 3;

#[link(name = "advapi32")]
extern "system" {
    fn RegCreateKeyExW(
        key: HKey,
        sub_key: *const u16,
        reserved: u32,
        class: *mut u16,
        options: u32,
        desired_access: u32,
        security_attributes: *const core::ffi::c_void,
        result: *mut HKey,
        disposition: *mut u32,
    ) -> LStatus;

    fn RegSetValueExW(
        key: HKey,
        value_name: *const u16,
        reserved: u32,
        value_type: u32,
        data: *const u8,
        data_size: u32,
    ) -> LStatus;

    fn RegDeleteKeyW(key: HKey, sub_key: *const u16) -> LStatus;

    fn RegCloseKey(key: HKey) -> LStatus;
}

#[derive(Debug, Eq, PartialEq)]
struct InstalledAppMetadata {
    display_name: &'static str,
    display_version: String,
    install_location: PathBuf,
    display_icon: PathBuf,
    uninstall_string: String,
}

fn installed_app_metadata(local_app_data: &Path, display_version: &str) -> InstalledAppMetadata {
    InstalledAppMetadata {
        display_name: "LCR",
        display_version: display_version.to_owned(),
        install_location: install_dir(local_app_data),
        display_icon: install_path(local_app_data),
        uninstall_string: uninstall_command(local_app_data),
    }
}

/// Registers LCR in Windows Installed Apps using the running Helper version.
pub fn register_installed_app(local_app_data: &Path) -> io::Result<()> {
    register_installed_app_version(local_app_data, CURRENT_VERSION)
}

/// Registers LCR in Windows Installed Apps using an explicitly proven committed
/// executable version.
///
/// U2 calls this only after the prepared PE ProductVersion matched the pending
/// version and canonical executable replacement has committed.
pub fn register_installed_app_version(
    local_app_data: &Path,
    committed_version: &str,
) -> io::Result<()> {
    if committed_version.is_empty() || committed_version.contains('\0') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid committed Helper version",
        ));
    }

    let metadata = installed_app_metadata(local_app_data, committed_version);
    let key = create_uninstall_key()?;

    set_reg_sz(key.0, "DisplayName", OsStr::new(metadata.display_name))?;
    set_reg_sz(
        key.0,
        "DisplayVersion",
        OsStr::new(&metadata.display_version),
    )?;
    set_reg_sz(
        key.0,
        "InstallLocation",
        metadata.install_location.as_os_str(),
    )?;
    set_reg_sz(key.0, "DisplayIcon", metadata.display_icon.as_os_str())?;
    set_reg_sz(
        key.0,
        "UninstallString",
        OsStr::new(&metadata.uninstall_string),
    )?;
    set_reg_dword(key.0, "NoModify", 1)?;
    set_reg_dword(key.0, "NoRepair", 1)?;

    Ok(())
}

/// Removes the current-user LCR Installed Apps registration.
///
/// Missing registration is success so clean uninstall remains restart-safe and
/// idempotent.
pub fn unregister_installed_app() -> io::Result<()> {
    delete_user_subkey(OsStr::new(UNINSTALL_REGISTRY_SUBKEY))
}

fn delete_user_subkey(sub_key: &OsStr) -> io::Result<()> {
    let sub_key = wide_nul(sub_key)?;

    let status = unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, sub_key.as_ptr()) };

    match status {
        0 | ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => Ok(()),
        status => Err(io::Error::from_raw_os_error(status)),
    }
}

struct RegistryKey(HKey);

impl Drop for RegistryKey {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

fn create_uninstall_key() -> io::Result<RegistryKey> {
    create_user_key(OsStr::new(UNINSTALL_REGISTRY_SUBKEY))
}

fn create_user_key(sub_key: &OsStr) -> io::Result<RegistryKey> {
    let sub_key = wide_nul(sub_key)?;
    let mut key = 0;

    let status = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            sub_key.as_ptr(),
            0,
            null_mut(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            null(),
            &mut key,
            null_mut(),
        )
    };

    win32_status(status)?;

    Ok(RegistryKey(key))
}

fn set_reg_sz(key: HKey, name: &str, value: &OsStr) -> io::Result<()> {
    let name = wide_nul(OsStr::new(name))?;
    let value = wide_nul(value)?;

    let byte_count = value
        .len()
        .checked_mul(size_of::<u16>())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "registry value too large"))?;

    let byte_count = u32::try_from(byte_count)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "registry value too large"))?;

    let status = unsafe {
        RegSetValueExW(
            key,
            name.as_ptr(),
            0,
            REG_SZ,
            value.as_ptr().cast::<u8>(),
            byte_count,
        )
    };

    win32_status(status)
}

fn set_reg_dword(key: HKey, name: &str, value: u32) -> io::Result<()> {
    let name = wide_nul(OsStr::new(name))?;

    let status = unsafe {
        RegSetValueExW(
            key,
            name.as_ptr(),
            0,
            REG_DWORD,
            (&value as *const u32).cast::<u8>(),
            size_of::<u32>() as u32,
        )
    };

    win32_status(status)
}

fn wide_nul(value: &OsStr) -> io::Result<Vec<u16>> {
    let mut wide = value.encode_wide().collect::<Vec<_>>();

    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Windows string contains NUL",
        ));
    }

    wide.push(0);
    Ok(wide)
}

fn win32_status(status: LStatus) -> io::Result<()> {
    if status == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(status))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installed_app_metadata_reuses_canonical_distribution_authority() {
        let local_app_data = Path::new("Local App Data");
        let metadata = installed_app_metadata(local_app_data, CURRENT_VERSION);

        assert_eq!(metadata.display_name, "LCR");
        assert_eq!(metadata.display_version, CURRENT_VERSION);
        assert_eq!(
            metadata.install_location,
            local_app_data.join("Programs").join("LCR")
        );
        assert_eq!(
            metadata.display_icon,
            local_app_data.join("Programs").join("LCR").join("LCR.exe")
        );
        assert_eq!(
            metadata.uninstall_string,
            format!(
                "\"{}\" --uninstall",
                local_app_data
                    .join("Programs")
                    .join("LCR")
                    .join("LCR.exe")
                    .display()
            )
        );
    }

    #[test]
    fn explicit_committed_version_metadata_does_not_use_running_version() {
        let local_app_data = Path::new("Local App Data");
        let metadata = installed_app_metadata(local_app_data, "9.8.7-beta.2");

        assert_eq!(metadata.display_version, "9.8.7-beta.2");
        assert_ne!(metadata.display_version, CURRENT_VERSION);
    }
    #[test]
    fn registry_subkey_removal_is_real_and_idempotent_in_isolated_hkcu_scope() {
        let sub_key = format!(
            r"Software\LinhLanBang.LCR.Helper.B3C1.Test.{}",
            std::process::id()
        );

        let key = create_user_key(OsStr::new(&sub_key)).unwrap();
        drop(key);

        delete_user_subkey(OsStr::new(&sub_key)).unwrap();
        delete_user_subkey(OsStr::new(&sub_key)).unwrap();
    }

    #[test]
    fn wide_nul_adds_exactly_one_terminator() {
        let encoded = wide_nul(OsStr::new("LCR")).unwrap();

        assert_eq!(encoded.last(), Some(&0));
        assert!(!encoded[..encoded.len() - 1].contains(&0));
    }

    #[test]
    fn wide_nul_rejects_interior_nul() {
        let error = wide_nul(OsStr::new("LCR\0invalid")).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }
}
