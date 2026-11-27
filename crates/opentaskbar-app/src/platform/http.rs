//! Single WinHTTP request core shared by update, geolocation and OAuth flows.
//!
//! Callers used to hand-write the same open → connect → request → send → read
//! chain with subtly different proxy/timeout/User-Agent/body-limit settings.
//! [`HttpCfg`] carries those differences as explicit parameters so behaviour
//! stays exactly as before while the unsafe handle plumbing lives in one place.

use std::io::{Read, Write};
use std::mem::size_of;
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use windows_sys::Win32::Networking::WinHttp::{
    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_ACCESS_TYPE_DEFAULT_PROXY,
    WINHTTP_ADDREQ_FLAG_ADD, WINHTTP_ADDREQ_FLAG_REPLACE, WINHTTP_FLAG_SECURE,
    WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE, WinHttpAddRequestHeaders,
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryDataAvailable,
    WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest,
    WinHttpSetTimeouts,
};

use super::win32_helpers::wide;

/// WinHTTP proxy mode. Different callers historically used different modes;
/// keep the choice visible instead of hard-coding one in the core.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Proxy {
    Automatic,
    Default,
}

/// Configuration for one HTTPS request.
#[derive(Clone, Debug)]
pub struct HttpCfg {
    pub agent: String,
    pub proxy: Proxy,
    /// resolve, connect, send, receive timeouts in milliseconds
    pub timeouts: [u32; 4],
    pub port: u16,
    pub secure: bool,
    /// `name: value` headers added with `WinHttpAddRequestHeaders`
    pub headers: Vec<(String, String)>,
    /// raw request body (POST/PUT), empty for GET
    pub body: Vec<u8>,
    /// stops reading after this many response bytes; `None` means unbounded
    pub max_response_bytes: Option<usize>,
}

impl HttpCfg {
    /// Sensible defaults for an HTTPS GET with the shared User-Agent.
    pub fn new(agent: impl AsRef<str>) -> Self {
        Self {
            agent: agent.as_ref().to_owned(),
            proxy: Proxy::Automatic,
            timeouts: [5_000, 5_000, 300_000, 300_000],
            port: 443,
            secure: true,
            headers: Vec::new(),
            body: Vec::new(),
            max_response_bytes: None,
        }
    }

    /// Five-minute send/receive budget used by the update downloader.
    pub fn download() -> Self {
        Self::new(concat!("OpenTaskbar/", env!("CARGO_PKG_VERSION")))
    }
}

/// Splits an absolute URL into `(host, port, secure, resource)`.
/// A bare `https://host` maps to resource `"/"`.
pub fn parse_url(url: &str) -> Option<(String, u16, bool, String)> {
    let (scheme, rest) = url.split_once("://")?;
    let secure = scheme.eq_ignore_ascii_case("https");
    let (authority, resource) = rest
        .split_once('/')
        .map(|(host, path)| (host, format!("/{path}")))
        .unwrap_or((rest, "/".to_owned()));
    let (host, explicit_port) = match authority.rsplit_once(':') {
        Some((host, port)) => (host, port.parse::<u16>().ok()),
        None => (authority, None),
    };
    if host.is_empty() {
        return None;
    }
    Some((
        host.to_owned(),
        explicit_port.unwrap_or(if secure { 443 } else { 80 }),
        secure,
        resource,
    ))
}

/// Performs one synchronous request, feeding every received chunk to `sink`.
/// Returns the HTTP status code when the request was sent successfully.
pub fn request<F: FnMut(&[u8])>(
    cfg: &HttpCfg,
    host: &str,
    path: &str,
    method: &str,
    mut sink: F,
) -> Option<u32> {
    let agent = wide(&cfg.agent);
    let host_w = wide(host);
    let path_w = wide(path);
    let verb_w = wide(method);
    let proxy = if cfg.proxy == Proxy::Automatic {
        WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY
    } else {
        WINHTTP_ACCESS_TYPE_DEFAULT_PROXY
    };
    let [resolve, connect_timeout, send, receive] = cfg.timeouts;
    unsafe {
        let session = WinHttpOpen(agent.as_ptr(), proxy, std::ptr::null(), std::ptr::null(), 0);
        if session.is_null() {
            return None;
        }
        WinHttpSetTimeouts(
            session,
            resolve as i32,
            connect_timeout as i32,
            send as i32,
            receive as i32,
        );
        let connect = WinHttpConnect(session, host_w.as_ptr(), cfg.port, 0);
        if connect.is_null() {
            WinHttpCloseHandle(session);
            return None;
        }
        let flags = if cfg.secure { WINHTTP_FLAG_SECURE } else { 0 };
        let request = WinHttpOpenRequest(
            connect,
            verb_w.as_ptr(),
            path_w.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            flags,
        );
        if request.is_null() {
            WinHttpCloseHandle(connect);
            WinHttpCloseHandle(session);
            return None;
        }
        if !cfg.headers.is_empty() {
            let header_text = cfg
                .headers
                .iter()
                .map(|(key, value)| format!("{key}: {value}\r\n"))
                .collect::<String>();
            let header_wide: Vec<u16> = header_text.encode_utf16().collect();
            WinHttpAddRequestHeaders(
                request,
                header_wide.as_ptr(),
                header_wide.len() as u32,
                WINHTTP_ADDREQ_FLAG_ADD | WINHTTP_ADDREQ_FLAG_REPLACE,
            );
        }
        let body_ptr = if cfg.body.is_empty() {
            std::ptr::null()
        } else {
            cfg.body.as_ptr().cast()
        };
        let body_len = cfg.body.len() as u32;
        let sent = WinHttpSendRequest(
            request,
            std::ptr::null(),
            0,
            body_ptr,
            body_len,
            body_len,
            0,
        ) != 0
            && WinHttpReceiveResponse(request, std::ptr::null_mut()) != 0;
        let result = if sent {
            let mut status = 0u32;
            let mut status_size = size_of::<u32>() as u32;
            WinHttpQueryHeaders(
                request,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                std::ptr::null(),
                (&mut status as *mut u32).cast(),
                &mut status_size,
                std::ptr::null_mut(),
            );
            let mut total = 0usize;
            'read: loop {
                let mut available = 0u32;
                if WinHttpQueryDataAvailable(request, &mut available) == 0 || available == 0 {
                    break;
                }
                if let Some(limit) = cfg.max_response_bytes
                    && total.saturating_add(available as usize) > limit
                {
                    break 'read;
                }
                let mut buffer = vec![0u8; available as usize];
                let mut read = 0u32;
                if WinHttpReadData(request, buffer.as_mut_ptr().cast(), available, &mut read) == 0
                    || read == 0
                {
                    break;
                }
                buffer.truncate(read as usize);
                total = total.saturating_add(buffer.len());
                sink(&buffer);
            }
            Some(status)
        } else {
            None
        };
        WinHttpCloseHandle(request);
        WinHttpCloseHandle(connect);
        WinHttpCloseHandle(session);
        result
    }
}

/// Convenience wrapper for callers that need the complete response body.
pub fn get_body(cfg: &HttpCfg, url: &str) -> Option<(u32, Vec<u8>)> {
    let (host, port, secure, path) = parse_url(url)?;
    let mut cfg = cfg.clone();
    cfg.port = port;
    cfg.secure = secure;
    let mut body = Vec::new();
    let status = request(&cfg, &host, &path, "GET", |chunk| {
        body.extend_from_slice(chunk)
    })?;
    Some((status, body))
}

/// One bounded HTTP/1.0 GET against an already validated loopback endpoint.
/// Used by vendor providers whose local agent speaks plain HTTP, not WinHTTP.
pub fn loopback_get(authority: &str, path: &str) -> Option<String> {
    let mut stream = TcpStream::connect_timeout(
        &authority.to_socket_addrs().ok()?.next()?,
        Duration::from_millis(300),
    )
    .ok()?;
    let _ = stream.set_read_timeout(Some(Duration::from_millis(400)));
    let request = format!("GET {path} HTTP/1.0\r\nHost: {authority}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).ok()?;
    let mut body = Vec::new();
    let mut chunk = [0u8; 8192];
    while body.len() < 256 * 1024 {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    let split = body.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = String::from_utf8_lossy(&body[..split]);
    if !head.starts_with("HTTP/1.") || !head.lines().next()?.contains(" 200 ") {
        return None;
    }
    Some(String::from_utf8_lossy(&body[split + 4..]).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_url_handles_ports_schemes_and_root_resources() {
        assert_eq!(
            parse_url("https://example.com/api/v1?q=1"),
            Some(("example.com".into(), 443, true, "/api/v1?q=1".into()))
        );
        assert_eq!(
            parse_url("https://example.com:8443"),
            Some(("example.com".into(), 8443, true, "/".into()))
        );
        assert_eq!(
            parse_url("http://127.0.0.1:9010/battery"),
            Some(("127.0.0.1".into(), 9010, false, "/battery".into()))
        );
        assert_eq!(parse_url("not-a-url"), None);
        assert_eq!(parse_url("https://"), None);
    }

    #[test]
    fn cfg_keeps_each_caller_policy_explicit() {
        let mut cfg = HttpCfg::new("OpenTaskbar/1.0");
        cfg.timeouts = [2_000, 2_000, 3_000, 4_000];
        cfg.max_response_bytes = Some(256 * 1024);
        assert_eq!(cfg.proxy, Proxy::Automatic);
        assert_eq!(cfg.timeouts[3], 4_000);
        assert_eq!(cfg.max_response_bytes, Some(262_144));
        let mut other = HttpCfg::download();
        other.proxy = Proxy::Default;
        assert_eq!(other.proxy, Proxy::Default);
        assert_eq!(other.port, 443);
    }
}
