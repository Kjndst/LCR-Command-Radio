use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

pub const DEFAULT_MANAGED_ENDPOINT: &str = "https://lcr.opik.net:6666";
pub const MANAGED_ENDPOINT_FILENAME: &str = "endpoint.txt";
pub const OVERRIDE_ENDPOINT_FILENAME: &str = "endpoint.override.txt";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EndpointSource {
    Override,
    Managed,
    CreatedManagedDefault,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedEndpoint {
    pub url: String,
    pub source: EndpointSource,
}

pub fn https_authority(endpoint: &str) -> Option<(&str, u16)> {
    let authority = endpoint.strip_prefix("https://")?;

    if authority.is_empty()
        || authority.contains('/')
        || authority.contains('?')
        || authority.contains('#')
        || authority.contains('@')
        || authority.chars().any(char::is_whitespace)
    {
        return None;
    }

    let Some((host, port_text)) = authority.rsplit_once(':') else {
        return Some((authority, 443));
    };

    if host.is_empty()
        || host.contains(':')
        || port_text.is_empty()
        || !port_text.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }

    let port = port_text.parse::<u16>().ok()?;

    if port == 0 || port.to_string() != port_text {
        return None;
    }

    Some((host, port))
}

pub fn normalize_endpoint(value: &str) -> Option<String> {
    let trimmed = value.trim();
    let normalized = trimmed.strip_suffix('/').unwrap_or(trimmed);

    https_authority(normalized)?;
    Some(normalized.to_owned())
}

pub fn paths_for_executable(executable: &Path) -> io::Result<(PathBuf, PathBuf)> {
    let directory = executable.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "Helper executable has no install directory",
        )
    })?;

    Ok((
        directory.join(MANAGED_ENDPOINT_FILENAME),
        directory.join(OVERRIDE_ENDPOINT_FILENAME),
    ))
}

fn invalid_endpoint(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("invalid LCR endpoint authority: {}", path.display()),
    )
}

fn read_endpoint(path: &Path) -> io::Result<String> {
    let text = fs::read_to_string(path)?;
    normalize_endpoint(&text).ok_or_else(|| invalid_endpoint(path))
}

fn read_existing_file(path: &Path) -> io::Result<Option<String>> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => read_endpoint(path).map(Some),
        Ok(_) => Err(invalid_endpoint(path)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn create_managed_default(path: &Path) -> io::Result<String> {
    let mut file = match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            return read_endpoint(path);
        }
        Err(error) => return Err(error),
    };

    file.write_all(DEFAULT_MANAGED_ENDPOINT.as_bytes())?;
    file.write_all(b"\r\n")?;
    file.sync_all()?;

    Ok(DEFAULT_MANAGED_ENDPOINT.to_owned())
}

pub fn resolve_for_executable(executable: &Path) -> io::Result<ResolvedEndpoint> {
    let (managed, override_path) = paths_for_executable(executable)?;

    if let Some(url) = read_existing_file(&override_path)? {
        return Ok(ResolvedEndpoint {
            url,
            source: EndpointSource::Override,
        });
    }

    if let Some(url) = read_existing_file(&managed)? {
        return Ok(ResolvedEndpoint {
            url,
            source: EndpointSource::Managed,
        });
    }

    let url = create_managed_default(&managed)?;
    Ok(ResolvedEndpoint {
        url,
        source: EndpointSource::CreatedManagedDefault,
    })
}

pub fn resolve_current_executable() -> io::Result<ResolvedEndpoint> {
    resolve_for_executable(&std::env::current_exe()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(case: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("lcr-helper-endpoint-{case}-{}", std::process::id()));

        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn missing_managed_file_materializes_lcr_opik_net() {
        let directory = test_dir("create");
        let executable = directory.join("LCR.exe");

        let resolved = resolve_for_executable(&executable).unwrap();

        assert_eq!(resolved.url, DEFAULT_MANAGED_ENDPOINT);
        assert_eq!(resolved.source, EndpointSource::CreatedManagedDefault);
        assert_eq!(
            fs::read_to_string(directory.join(MANAGED_ENDPOINT_FILENAME))
                .unwrap()
                .trim(),
            DEFAULT_MANAGED_ENDPOINT
        );

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn managed_file_is_used_when_override_is_absent() {
        let directory = test_dir("managed");
        let executable = directory.join("LCR.exe");
        fs::write(
            directory.join(MANAGED_ENDPOINT_FILENAME),
            b"https://managed.example\r\n",
        )
        .unwrap();

        let resolved = resolve_for_executable(&executable).unwrap();

        assert_eq!(resolved.url, "https://managed.example");
        assert_eq!(resolved.source, EndpointSource::Managed);

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn advanced_override_wins_without_mutating_managed_file() {
        let directory = test_dir("override");
        let executable = directory.join("LCR.exe");
        let managed = directory.join(MANAGED_ENDPOINT_FILENAME);
        fs::write(&managed, b"https://managed.example\r\n").unwrap();
        fs::write(
            directory.join(OVERRIDE_ENDPOINT_FILENAME),
            b"https://debug.example\r\n",
        )
        .unwrap();

        let resolved = resolve_for_executable(&executable).unwrap();

        assert_eq!(resolved.url, "https://debug.example");
        assert_eq!(resolved.source, EndpointSource::Override);
        assert_eq!(
            fs::read_to_string(managed).unwrap().trim(),
            "https://managed.example"
        );

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn malformed_explicit_authority_fails_closed() {
        let directory = test_dir("invalid");
        let executable = directory.join("LCR.exe");
        fs::write(
            directory.join(MANAGED_ENDPOINT_FILENAME),
            b"https://managed.example\r\n",
        )
        .unwrap();
        fs::write(
            directory.join(OVERRIDE_ENDPOINT_FILENAME),
            b"http://not-https.example\r\n",
        )
        .unwrap();

        let error = resolve_for_executable(&executable).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn endpoint_shape_remains_strict_with_optional_explicit_port() {
        assert_eq!(
            normalize_endpoint(" https://lcr.opik.net:6666/ "),
            Some("https://lcr.opik.net:6666".into())
        );
        assert_eq!(
            https_authority("https://lcr.opik.net:6666"),
            Some(("lcr.opik.net", 6666))
        );
        assert_eq!(
            https_authority("https://lcr.opik.net"),
            Some(("lcr.opik.net", 443))
        );
        assert_eq!(
            normalize_endpoint("https://example.com:443"),
            Some("https://example.com:443".into())
        );
        assert_eq!(normalize_endpoint("http://example.com"), None);
        assert_eq!(normalize_endpoint("https://example.com/path"), None);
        assert_eq!(normalize_endpoint("https://example.com:0"), None);
        assert_eq!(normalize_endpoint("https://example.com:65536"), None);
        assert_eq!(normalize_endpoint("https://example.com:06666"), None);
        assert_eq!(normalize_endpoint("https://example.com:not-a-port"), None);
        assert_eq!(normalize_endpoint("https://user@example.com:6666"), None);
        assert_eq!(
            normalize_endpoint("https://one.example\nhttps://two.example"),
            None
        );
    }
}
