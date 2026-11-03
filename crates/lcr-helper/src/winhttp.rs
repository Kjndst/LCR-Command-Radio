//! Native WinHTTP handle owners.
//!
//! `WinHttp` remains the existing control-worker transport. `PublicUpdateHttp`
//! is a separate, credential-free, bounded session used only by updater U1.

#[cfg(windows)]
use std::{io, iter::once, mem::size_of, ptr::null};

#[cfg(windows)]
type Handle = *mut core::ffi::c_void;
#[cfg(windows)]
#[link(name = "winhttp")]
extern "system" {
    fn WinHttpOpen(
        agent: *const u16,
        access: u32,
        proxy: *const u16,
        bypass: *const u16,
        flags: u32,
    ) -> Handle;
    fn WinHttpConnect(session: Handle, server: *const u16, port: u16, reserved: u32) -> Handle;
    fn WinHttpSetTimeouts(
        handle: Handle,
        resolve: i32,
        connect: i32,
        send: i32,
        receive: i32,
    ) -> i32;
    fn WinHttpSetOption(
        handle: Handle,
        option: u32,
        buffer: *mut core::ffi::c_void,
        length: u32,
    ) -> i32;
    fn WinHttpCloseHandle(handle: Handle) -> i32;
    fn WinHttpOpenRequest(
        connection: Handle,
        verb: *const u16,
        object: *const u16,
        version: *const u16,
        referer: *const u16,
        accept: *const *const u16,
        flags: u32,
    ) -> Handle;
    fn WinHttpAddRequestHeaders(
        request: Handle,
        headers: *const u16,
        length: u32,
        modifiers: u32,
    ) -> i32;
    fn WinHttpSendRequest(
        request: Handle,
        headers: *const u16,
        header_len: u32,
        body: *const core::ffi::c_void,
        body_len: u32,
        total: u32,
        context: usize,
    ) -> i32;
    fn WinHttpReceiveResponse(request: Handle, reserved: *mut core::ffi::c_void) -> i32;
    fn WinHttpQueryHeaders(
        request: Handle,
        level: u32,
        name: *const u16,
        buffer: *mut core::ffi::c_void,
        length: *mut u32,
        index: *mut u32,
    ) -> i32;
    fn WinHttpReadData(
        request: Handle,
        buffer: *mut core::ffi::c_void,
        length: u32,
        read: *mut u32,
    ) -> i32;
}

#[cfg(windows)]
const WINHTTP_FLAG_SECURE: u32 = 0x0080_0000;
#[cfg(windows)]
const WINHTTP_QUERY_STATUS_CODE_NUMERIC: u32 = 19 | 0x2000_0000;
#[cfg(windows)]
const WINHTTP_OPTION_REDIRECT_POLICY: u32 = 88;
#[cfg(windows)]
const WINHTTP_OPTION_MAX_HTTP_AUTOMATIC_REDIRECTS: u32 = 89;
#[cfg(windows)]
const WINHTTP_OPTION_REDIRECT_POLICY_DISALLOW_HTTPS_TO_HTTP: u32 = 1;
#[cfg(windows)]
const UPDATER_MAX_AUTOMATIC_REDIRECTS: u32 = 5;
#[cfg(windows)]
const UPDATER_RESOLVE_TIMEOUT_MS: i32 = 5_000;
#[cfg(windows)]
const UPDATER_CONNECT_TIMEOUT_MS: i32 = 5_000;
#[cfg(windows)]
const UPDATER_SEND_TIMEOUT_MS: i32 = 5_000;
#[cfg(windows)]
const UPDATER_RECEIVE_TIMEOUT_MS: i32 = 15_000;

#[cfg(windows)]
struct OwnedInternetHandle(Handle);

#[cfg(windows)]
impl OwnedInternetHandle {
    fn from_raw(handle: Handle, operation: &'static str) -> io::Result<Self> {
        if handle.is_null() {
            Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                format!("{operation} failed: {}", io::Error::last_os_error()),
            ))
        } else {
            Ok(Self(handle))
        }
    }
}

#[cfg(windows)]
impl Drop for OwnedInternetHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

/// Credential-free WinHTTP session dedicated to U1 update discovery/download.
///
/// It has no device-token API and is not shared with the PTT/control worker.
#[cfg(windows)]
pub(crate) struct PublicUpdateHttp {
    session: OwnedInternetHandle,
}

#[cfg(windows)]
impl PublicUpdateHttp {
    pub(crate) fn open() -> io::Result<Self> {
        let agent = wide_nul("LCR-Updater")?;

        let session = unsafe {
            OwnedInternetHandle::from_raw(
                WinHttpOpen(agent.as_ptr(), 0, null(), null(), 0),
                "WinHttpOpen updater session",
            )?
        };

        let timeouts_ok = unsafe {
            WinHttpSetTimeouts(
                session.0,
                UPDATER_RESOLVE_TIMEOUT_MS,
                UPDATER_CONNECT_TIMEOUT_MS,
                UPDATER_SEND_TIMEOUT_MS,
                UPDATER_RECEIVE_TIMEOUT_MS,
            )
        };
        if timeouts_ok == 0 {
            return Err(io::Error::last_os_error());
        }

        // Every U1 request starts as HTTPS. This explicit WinHTTP policy makes
        // an HTTPS -> HTTP redirect fail rather than silently downgrade.
        let mut redirect_policy = WINHTTP_OPTION_REDIRECT_POLICY_DISALLOW_HTTPS_TO_HTTP;
        let redirect_policy_ok = unsafe {
            WinHttpSetOption(
                session.0,
                WINHTTP_OPTION_REDIRECT_POLICY,
                (&mut redirect_policy as *mut u32).cast(),
                size_of::<u32>() as u32,
            )
        };
        if redirect_policy_ok == 0 {
            return Err(io::Error::last_os_error());
        }

        let mut max_redirects = UPDATER_MAX_AUTOMATIC_REDIRECTS;
        let max_redirects_ok = unsafe {
            WinHttpSetOption(
                session.0,
                WINHTTP_OPTION_MAX_HTTP_AUTOMATIC_REDIRECTS,
                (&mut max_redirects as *mut u32).cast(),
                size_of::<u32>() as u32,
            )
        };
        if max_redirects_ok == 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(Self { session })
    }

    fn get_stream(
        &self,
        host: &str,
        path: &str,
        headers: Option<&str>,
        max_bytes: u64,
        sink: &mut dyn FnMut(&[u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        if host.is_empty()
            || host.contains(['/', '\\', '@', ':'])
            || host.chars().any(char::is_whitespace)
            || !path.starts_with('/')
            || path.contains('\0')
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid updater WinHTTP authority",
            ));
        }

        let server = wide_nul(host)?;
        let verb = wide_nul("GET")?;
        let object = wide_nul(path)?;

        let connection = unsafe {
            OwnedInternetHandle::from_raw(
                WinHttpConnect(self.session.0, server.as_ptr(), 443, 0),
                "WinHttpConnect updater",
            )?
        };

        let request = unsafe {
            OwnedInternetHandle::from_raw(
                WinHttpOpenRequest(
                    connection.0,
                    verb.as_ptr(),
                    object.as_ptr(),
                    null(),
                    null(),
                    null(),
                    WINHTTP_FLAG_SECURE,
                ),
                "WinHttpOpenRequest updater",
            )?
        };

        if let Some(headers) = headers {
            let headers = wide_nul(headers)?;
            let added =
                unsafe { WinHttpAddRequestHeaders(request.0, headers.as_ptr(), u32::MAX, 0) };
            if added == 0 {
                return Err(io::Error::last_os_error());
            }
        }

        let sent = unsafe { WinHttpSendRequest(request.0, null(), 0, core::ptr::null(), 0, 0, 0) };
        if sent == 0 {
            return Err(io::Error::last_os_error());
        }

        let received = unsafe { WinHttpReceiveResponse(request.0, core::ptr::null_mut()) };
        if received == 0 {
            return Err(io::Error::last_os_error());
        }

        let mut status = 0u32;
        let mut status_len = size_of::<u32>() as u32;
        let queried = unsafe {
            WinHttpQueryHeaders(
                request.0,
                WINHTTP_QUERY_STATUS_CODE_NUMERIC,
                null(),
                (&mut status as *mut u32).cast(),
                &mut status_len,
                core::ptr::null_mut(),
            )
        };
        if queried == 0 {
            return Err(io::Error::last_os_error());
        }

        if status != 200 {
            return Err(io::Error::other(format!(
                "unexpected updater HTTP status {status}"
            )));
        }

        let mut total = 0u64;
        loop {
            let mut buffer = [0u8; 8192];
            let mut read = 0u32;

            let read_ok = unsafe {
                WinHttpReadData(
                    request.0,
                    buffer.as_mut_ptr().cast(),
                    buffer.len() as u32,
                    &mut read,
                )
            };
            if read_ok == 0 {
                return Err(io::Error::last_os_error());
            }
            if read == 0 {
                break;
            }

            let read = usize::try_from(read).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid WinHTTP read size")
            })?;
            let new_total = total.checked_add(read as u64).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "WinHTTP size overflow")
            })?;

            if new_total > max_bytes {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "updater response exceeded bounded size",
                ));
            }

            sink(&buffer[..read])?;
            total = new_total;
        }

        Ok(())
    }
}

#[cfg(windows)]
impl crate::updater::UpdateNetwork for PublicUpdateHttp {
    fn fetch_releases(&mut self) -> io::Result<Vec<u8>> {
        const GITHUB_API_HEADERS: &str =
            "Accept: application/vnd.github+json\r\nX-GitHub-Api-Version: 2022-11-28\r\n";

        let mut body = Vec::new();

        self.get_stream(
            crate::updater::RELEASE_API_HOST,
            crate::updater::RELEASE_API_PATH,
            Some(GITHUB_API_HEADERS),
            crate::updater::MAX_RELEASE_API_BYTES,
            &mut |chunk| {
                body.extend_from_slice(chunk);
                Ok(())
            },
        )?;

        Ok(body)
    }

    fn download_candidate(
        &mut self,
        candidate: &crate::updater::ReleaseCandidate,
        sink: &mut dyn FnMut(&[u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        let path = crate::updater::release_download_path(candidate).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "updater candidate URL is not fixed public GitHub authority",
            )
        })?;

        self.get_stream(
            crate::updater::RELEASE_DOWNLOAD_HOST,
            &path,
            None,
            candidate.size,
            sink,
        )
    }
}

#[cfg(windows)]
fn wide_nul(value: &str) -> io::Result<Vec<u16>> {
    if value.contains('\0') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "WinHTTP string contains NUL",
        ));
    }

    Ok(value.encode_utf16().chain(once(0)).collect())
}

#[cfg(all(test, windows))]
mod updater_http_tests {
    use super::*;

    #[test]
    fn updater_public_session_accepts_locked_timeout_and_redirect_options() {
        // WinHttpOpen + WinHttpSetTimeouts + both WinHttpSetOption calls only.
        // No connection and no HTTP request is made by this proof.
        PublicUpdateHttp::open().unwrap();
    }
    #[test]
    fn updater_redirect_policy_is_https_downgrade_blocking_and_bounded() {
        assert_eq!(WINHTTP_OPTION_REDIRECT_POLICY, 88);
        assert_eq!(WINHTTP_OPTION_REDIRECT_POLICY_DISALLOW_HTTPS_TO_HTTP, 1);
        assert_eq!(WINHTTP_OPTION_MAX_HTTP_AUTOMATIC_REDIRECTS, 89);
        assert_eq!(UPDATER_MAX_AUTOMATIC_REDIRECTS, 5);
    }
}
#[cfg(windows)]
pub struct WinHttp {
    session: Handle,
    connection: Handle,
    probe_required: bool,
}
#[cfg(windows)]
impl WinHttp {
    pub fn open(host: &str, port: u16) -> Result<Self, ()> {
        let agent: Vec<u16> = "LCR".encode_utf16().chain(once(0)).collect();
        let server: Vec<u16> = host.encode_utf16().chain(once(0)).collect();
        unsafe {
            let session = WinHttpOpen(agent.as_ptr(), 0, null(), null(), 0);
            if session.is_null() {
                return Err(());
            }
            if WinHttpSetTimeouts(session, 2000, 2000, 2200, 2200) == 0 {
                WinHttpCloseHandle(session);
                return Err(());
            }
            let connection = WinHttpConnect(session, server.as_ptr(), port, 0);
            if connection.is_null() {
                WinHttpCloseHandle(session);
                return Err(());
            }
            Ok(Self {
                session,
                connection,
                probe_required: false,
            })
        }
    }
    pub fn post(&self, path: &str, body: &[u8], token: Option<&str>) -> Result<(u16, Vec<u8>), ()> {
        const SECURE: u32 = 0x0080_0000;
        const STATUS: u32 = 19 | 0x2000_0000;
        const MAX: usize = 8192;
        let wide = |s: &str| s.encode_utf16().chain(once(0)).collect::<Vec<_>>();
        let verb = wide("POST");
        let path = wide(path);
        unsafe {
            let request = WinHttpOpenRequest(
                self.connection,
                verb.as_ptr(),
                path.as_ptr(),
                null(),
                null(),
                null(),
                SECURE,
            );
            if request.is_null() {
                return Err(());
            }
            let result = (|| {
                let mut headers = "Content-Type: application/json\r\n".to_owned();
                if let Some(token) = token {
                    headers.push_str("Authorization: Bearer ");
                    headers.push_str(token);
                    headers.push_str("\r\n");
                }
                let headers = wide(&headers);
                if WinHttpAddRequestHeaders(request, headers.as_ptr(), u32::MAX, 0) == 0
                    || WinHttpSendRequest(
                        request,
                        null(),
                        0,
                        body.as_ptr().cast(),
                        body.len() as u32,
                        body.len() as u32,
                        0,
                    ) == 0
                    || WinHttpReceiveResponse(request, core::ptr::null_mut()) == 0
                {
                    return Err(());
                }
                let mut status = 0u32;
                let mut len = 4u32;
                if WinHttpQueryHeaders(
                    request,
                    STATUS,
                    null(),
                    (&mut status as *mut u32).cast(),
                    &mut len,
                    core::ptr::null_mut(),
                ) == 0
                {
                    return Err(());
                }
                let mut out = Vec::new();
                loop {
                    let mut buf = [0u8; 1024];
                    let mut read = 0;
                    if WinHttpReadData(
                        request,
                        buf.as_mut_ptr().cast(),
                        buf.len() as u32,
                        &mut read,
                    ) == 0
                    {
                        return Err(());
                    }
                    if read == 0 {
                        break;
                    }
                    if out.len() + read as usize > MAX {
                        return Err(());
                    }
                    out.extend_from_slice(&buf[..read as usize]);
                }
                Ok((status as u16, out))
            })();
            WinHttpCloseHandle(request);
            result
        }
    }

    pub(crate) fn with_credential_probe(mut self) -> Self {
        self.probe_required = true;
        self
    }

    /// Existing heartbeat endpoint, separate read-only probe schema.
    /// Ordinary heartbeat can renew OPEN, so never use it for cold-start
    /// candidate authentication or send state before the active proof.
    pub(crate) fn previously_activated(&self, token: &str) -> Result<bool, ()> {
        let (status, body) = self.post("/v1/heartbeat", br#"{"probe":true}"#, Some(token))?;
        match crate::transport::classify_credential_probe(status, &body) {
            crate::transport::CredentialProbe::Active => Ok(true),
            crate::transport::CredentialProbe::Rejected => Ok(false),
            crate::transport::CredentialProbe::Transient => Err(()),
        }
    }

    pub(crate) fn pair(&self, code: &str) -> crate::protocol::PairReply {
        let payload = crate::protocol::pair_request(code);
        let body = match serde_json::to_vec(&payload) {
            Ok(body) => body,
            Err(_) => return crate::protocol::PairReply::Transient,
        };

        match self.post("/v1/pair", &body, None) {
            Ok((status, response_body)) => crate::protocol::classify_pair(status, &response_body),
            Err(()) => crate::protocol::PairReply::Transient,
        }
    }
}
#[cfg(windows)]
impl Drop for WinHttp {
    fn drop(&mut self) {
        unsafe {
            WinHttpCloseHandle(self.connection);
            WinHttpCloseHandle(self.session);
        }
    }
}

#[cfg(windows)]
impl crate::worker::ControlTransport for WinHttp {
    fn execute(
        &mut self,
        action: crate::reconciler::Action,
        device_token: Option<&str>,
    ) -> crate::transport::ControlOutcome {
        use crate::reconciler::{Action, Desired};
        use crate::transport::ControlOutcome;

        match action {
            Action::None | Action::InvalidatePairing => ControlOutcome::Idle,

            Action::SendState {
                desired,
                generation,
            } => {
                let Some(token) = device_token else {
                    return ControlOutcome::Unauthorized;
                };
                if self.probe_required {
                    match self.previously_activated(token) {
                        Ok(true) => self.probe_required = false,
                        Ok(false) => return ControlOutcome::Unauthorized,
                        Err(()) => return ControlOutcome::Transient,
                    }
                }

                let payload =
                    crate::protocol::state_request(matches!(desired, Desired::Open), generation);

                let body = match serde_json::to_vec(&payload) {
                    Ok(body) => body,
                    Err(_) => return ControlOutcome::Transient,
                };

                let (status, response_body) = match self.post("/v1/state", &body, Some(token)) {
                    Ok(response) => response,
                    Err(()) => return ControlOutcome::Transient,
                };

                crate::transport::classify(status, &response_body, desired, generation)
            }

            Action::Heartbeat { generation } => {
                let Some(token) = device_token else {
                    return ControlOutcome::Unauthorized;
                };

                let payload = crate::protocol::heartbeat_request(generation);

                let body = match serde_json::to_vec(&payload) {
                    Ok(body) => body,
                    Err(_) => return ControlOutcome::Transient,
                };

                let (status, response_body) = match self.post("/v1/heartbeat", &body, Some(token)) {
                    Ok(response) => response,
                    Err(()) => return ControlOutcome::Transient,
                };

                crate::transport::classify_heartbeat(status, &response_body, generation)
            }
        }
    }
}
