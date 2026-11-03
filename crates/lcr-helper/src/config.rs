use serde_json::json;
use std::{
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

pub const DEFAULT_HEARTBEAT_HINT_MS: u64 = 250;

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Eq, PartialEq)]
pub struct Config {
    pub device_token: Option<String>,
    /// Newly delivered credential; server activation is not yet durably acknowledged.
    pub pending_pair: bool,
    pub radio_key: String,
    pub radio_key_kind: String,
    pub radio_key_code: u32,
    pub ptt_sound: bool,
    pub generation: u64,
    pub heartbeat_hint_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            device_token: None,
            pending_pair: false,
            radio_key: "Mouse5".into(),
            radio_key_kind: "mouse".into(),
            radio_key_code: 0,
            ptt_sound: false,
            generation: 0,
            heartbeat_hint_ms: DEFAULT_HEARTBEAT_HINT_MS,
        }
    }
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("paired", &self.device_token.is_some())
            .field("generation", &self.generation)
            .finish()
    }
}

pub trait Store {
    fn save(&mut self, config: &Config) -> Result<(), ()>;
}

pub struct FileStore {
    path: PathBuf,
}

impl FileStore {
    pub fn appdata_path(appdata: &str) -> PathBuf {
        PathBuf::from(appdata).join("LCR").join("config.json")
    }

    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn load(&self) -> Config {
        let Ok(bytes) = fs::read(&self.path) else {
            return Config::default();
        };

        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            return Config::default();
        };

        Config {
            device_token: value
                .get("device_token")
                .and_then(|v| v.as_str())
                .map(str::to_owned),
            pending_pair: value
                .get("pending_pair")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            radio_key: value
                .get("radio_key")
                .and_then(|v| v.as_str())
                .unwrap_or("Mouse5")
                .to_owned(),
            radio_key_kind: value
                .get("radio_key_kind")
                .and_then(|v| v.as_str())
                .unwrap_or("mouse")
                .to_owned(),
            radio_key_code: value
                .get("radio_key_code")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32,
            ptt_sound: value
                .get("ptt_sound")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            generation: value
                .get("generation")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            heartbeat_hint_ms: value
                .get("heartbeat_hint_ms")
                .and_then(|v| v.as_u64())
                .filter(|value| *value > 0)
                .unwrap_or(DEFAULT_HEARTBEAT_HINT_MS),
        }
    }
}

impl Store for FileStore {
    fn save(&mut self, config: &Config) -> Result<(), ()> {
        let bytes = serde_json::to_vec(&json!({
            "device_token": &config.device_token,
            "pending_pair": config.pending_pair,
            "radio_key": &config.radio_key,
            "radio_key_kind": &config.radio_key_kind,
            "radio_key_code": config.radio_key_code,
            "ptt_sound": config.ptt_sound,
            "generation": config.generation,
            "heartbeat_hint_ms": config.heartbeat_hint_ms
        }))
        .map_err(|_| ())?;

        atomic_write(&self.path, &bytes)
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), ()> {
    atomic_write_with(path, bytes, replace_file)
}

fn atomic_write_with(
    path: &Path,
    bytes: &[u8],
    replace: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> Result<(), ()> {
    let directory = path.parent().ok_or(())?;
    fs::create_dir_all(directory).map_err(|_| ())?;

    let (temporary, mut file) = create_temporary(directory, path).map_err(|_| ())?;

    let result = (|| -> io::Result<()> {
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);

        replace(&temporary, path)?;
        sync_parent_directory(directory)?;

        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }

    result.map_err(|_| ())
}

fn create_temporary(directory: &Path, target: &Path) -> io::Result<(PathBuf, File)> {
    let name = target
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing config filename"))?
        .to_string_lossy();

    for _ in 0..32 {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temporary = directory.join(format!(".{name}.{}.{}.tmp", std::process::id(), sequence));

        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => return Ok((temporary, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "unable to allocate helper config temporary file",
    ))
}

#[cfg(windows)]
fn replace_file(source: &Path, target: &Path) -> io::Result<()> {
    use std::{iter::once, os::windows::ffi::OsStrExt};

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x0000_0001;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x0000_0008;

    #[link(name = "kernel32")]
    extern "system" {
        fn MoveFileExW(
            existing_file_name: *const u16,
            new_file_name: *const u16,
            flags: u32,
        ) -> i32;
    }

    let source = source
        .as_os_str()
        .encode_wide()
        .chain(once(0))
        .collect::<Vec<_>>();

    let target = target
        .as_os_str()
        .encode_wide()
        .chain(once(0))
        .collect::<Vec<_>>();

    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            target.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };

    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_file(source: &Path, target: &Path) -> io::Result<()> {
    fs::rename(source, target)
}

#[cfg(unix)]
fn sync_parent_directory(directory: &Path) -> io::Result<()> {
    File::open(directory)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent_directory(_directory: &Path) -> io::Result<()> {
    // Windows replacement already uses MOVEFILE_WRITE_THROUGH.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "lcr-helper-config-{name}-{}.json",
            std::process::id()
        ))
    }

    #[test]
    fn file_store_round_trips_pairing_and_generation() {
        let path = test_path("roundtrip");
        let _ = fs::remove_file(&path);

        let mut store = FileStore::new(path.clone());

        let config = Config {
            device_token: Some("pair-token".into()),
            pending_pair: true,
            ptt_sound: true,
            generation: 9,
            heartbeat_hint_ms: 275,
            ..Config::default()
        };

        store.save(&config).unwrap();
        assert_eq!(store.load(), config);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn existing_helper_json_schema_remains_compatible() {
        let path = test_path("legacy-schema");
        let _ = fs::remove_file(&path);

        fs::write(
            &path,
            br#"{
                "server_url":"https://example.test",
                "device_token":"legacy-token",
                "radio_key":"Mouse4",
                "radio_key_kind":"mouse",
                "radio_key_code":0,
                "generation":41,
                "heartbeat_hint_ms":275
            }"#,
        )
        .unwrap();

        let config = FileStore::new(path.clone()).load();

        // Legacy server_url is deliberately accepted syntactically but ignored:
        // endpoint sidecars beside LCR.exe are the only endpoint authority.
        assert_eq!(config.device_token.as_deref(), Some("legacy-token"));
        assert!(!config.pending_pair);
        assert_eq!(config.radio_key, "Mouse4");
        assert_eq!(config.radio_key_kind, "mouse");
        assert_eq!(config.radio_key_code, 0);
        assert!(!config.ptt_sound);
        assert_eq!(config.generation, 41);
        assert_eq!(config.heartbeat_hint_ms, 275);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn failed_atomic_replace_leaves_existing_config_intact() {
        let path = test_path("replace-failure");
        let _ = fs::remove_file(&path);

        let original = b"existing-authority";
        fs::write(&path, original).unwrap();

        let result = atomic_write_with(&path, b"replacement", |_, _| {
            Err(io::Error::other("injected replacement failure"))
        });

        assert_eq!(result, Err(()));
        assert_eq!(fs::read(&path).unwrap(), original);

        let _ = fs::remove_file(path);
    }
}
