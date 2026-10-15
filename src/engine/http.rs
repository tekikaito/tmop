//! WinHTTP client for the engine downloads and the update check: GET to a file with progress and
//! cancellation, bounded text fetch, and HEAD status. Redirects are followed, proxies are automatic.

use std::ffi::c_void;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use windows::core::PCWSTR;
use windows::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpCrackUrl, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders, WinHttpReadData,
    WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetOption, WinHttpSetTimeouts, URL_COMPONENTS, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
    WINHTTP_FLAG_SECURE, WINHTTP_INTERNET_SCHEME_HTTP, WINHTTP_INTERNET_SCHEME_HTTPS, WINHTTP_OPTION_REDIRECT_POLICY,
    WINHTTP_OPTION_REDIRECT_POLICY_ALWAYS, WINHTTP_QUERY_CONTENT_LENGTH, WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
};

use crate::pro::winutil::wide;

/// Read size for response bodies.
const CHUNK: usize = 256 * 1024;

/// An owned WinHTTP handle, closed on drop.
struct Handle(*mut c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the handle came from WinHttpOpen/Connect/OpenRequest and is closed once.
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

/// Session, connection and request of one transfer. Fields drop in this order: request first.
struct Transfer {
    request: Handle,
    _connect: Handle,
    _session: Handle,
}

fn win_error(what: &str, e: windows::core::Error) -> String {
    format!("{what} failed: {e}")
}

/// Splits `url` into host, port, path (with query) and the https flag.
fn crack(url: &str) -> Result<(String, u16, String, bool), String> {
    let url_w = wide(url);
    let text = &url_w[..url_w.len() - 1];
    // SAFETY: all component lengths are set to u32::MAX, so WinHTTP points into `text`.
    let mut parts: URL_COMPONENTS = unsafe { std::mem::zeroed() };
    parts.dwStructSize = std::mem::size_of::<URL_COMPONENTS>() as u32;
    parts.dwSchemeLength = u32::MAX;
    parts.dwHostNameLength = u32::MAX;
    parts.dwUrlPathLength = u32::MAX;
    parts.dwExtraInfoLength = u32::MAX;
    // SAFETY: `text` and `parts` are valid for the call.
    unsafe { WinHttpCrackUrl(text, 0, &mut parts) }.map_err(|e| format!("invalid URL {url}: {e}"))?;

    let secure = if parts.nScheme == WINHTTP_INTERNET_SCHEME_HTTPS {
        true
    } else if parts.nScheme == WINHTTP_INTERNET_SCHEME_HTTP {
        false
    } else {
        return Err(format!("unsupported URL scheme: {url}"));
    };
    let host = utf16_at(parts.lpszHostName.0, parts.dwHostNameLength);
    let mut path = utf16_at(parts.lpszUrlPath.0, parts.dwUrlPathLength);
    path.push_str(&utf16_at(parts.lpszExtraInfo.0, parts.dwExtraInfoLength));
    if path.is_empty() {
        path.push('/');
    }
    Ok((host, parts.nPort, path, secure))
}

/// Copies `len` UTF-16 units from a pointer returned by WinHttpCrackUrl.
fn utf16_at(ptr: *mut u16, len: u32) -> String {
    if ptr.is_null() || len == 0 {
        return String::new();
    }
    // SAFETY: WinHTTP guarantees `len` units at `ptr` inside the cracked URL buffer.
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(ptr, len as usize) })
}

/// Opens session, connection and request for `verb` on `url` and sends it.
/// Returns the transfer and the HTTP status code (any status, not only 200).
fn send(url: &str, verb: &str) -> Result<(Transfer, u32), String> {
    let (host, port, path, secure) = crack(url)?;
    let agent = wide(&format!("TMOP/{}", env!("CARGO_PKG_VERSION")));
    let host_w = wide(&host);
    let verb_w = wide(verb);
    let path_w = wide(&path);

    // SAFETY: every pointer passed is NUL-terminated and alive for the call; null results are checked.
    unsafe {
        let session = Handle(WinHttpOpen(PCWSTR(agent.as_ptr()), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0));
        if session.0.is_null() {
            return Err(win_error("WinHttpOpen", windows::core::Error::from_win32()));
        }
        // Resolve and connect 30 s, send and receive 60 s.
        WinHttpSetTimeouts(session.0, 30_000, 30_000, 60_000, 60_000)
            .map_err(|e| win_error("WinHttpSetTimeouts", e))?;

        let connect = Handle(WinHttpConnect(session.0, PCWSTR(host_w.as_ptr()), port, 0));
        if connect.0.is_null() {
            return Err(win_error("WinHttpConnect", windows::core::Error::from_win32()));
        }
        let flags = if secure { WINHTTP_FLAG_SECURE } else { Default::default() };
        let request = Handle(WinHttpOpenRequest(
            connect.0,
            PCWSTR(verb_w.as_ptr()),
            PCWSTR(path_w.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            flags,
        ));
        if request.0.is_null() {
            return Err(win_error("WinHttpOpenRequest", windows::core::Error::from_win32()));
        }
        let policy = WINHTTP_OPTION_REDIRECT_POLICY_ALWAYS.to_ne_bytes();
        WinHttpSetOption(Some(request.0), WINHTTP_OPTION_REDIRECT_POLICY, Some(&policy))
            .map_err(|e| win_error("WinHttpSetOption", e))?;

        WinHttpSendRequest(request.0, None, None, 0, 0, 0).map_err(|e| win_error("WinHttpSendRequest", e))?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut()).map_err(|e| win_error("WinHttpReceiveResponse", e))?;

        let mut status = 0u32;
        let mut len = std::mem::size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut c_void),
            &mut len,
            std::ptr::null_mut(),
        )
        .map_err(|e| win_error("WinHttpQueryHeaders(status)", e))?;

        Ok((Transfer { request, _connect: connect, _session: session }, status))
    }
}

/// Content-Length of the response, or 0 when the server sent none.
fn content_length(t: &Transfer) -> u64 {
    let mut value = 0u32;
    let mut len = std::mem::size_of::<u32>() as u32;
    // SAFETY: the buffer holds one u32, as requested by WINHTTP_QUERY_FLAG_NUMBER.
    let ok = unsafe {
        WinHttpQueryHeaders(
            t.request.0,
            WINHTTP_QUERY_CONTENT_LENGTH | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut value as *mut u32 as *mut c_void),
            &mut len,
            std::ptr::null_mut(),
        )
    };
    if ok.is_ok() {
        value as u64
    } else {
        0
    }
}

fn read_chunk(t: &Transfer, buf: &mut [u8]) -> Result<usize, String> {
    let mut read = 0u32;
    // SAFETY: `buf` is valid for `buf.len()` bytes (at most CHUNK, which fits u32).
    unsafe { WinHttpReadData(t.request.0, buf.as_mut_ptr() as *mut c_void, buf.len() as u32, &mut read) }
        .map_err(|e| win_error("WinHttpReadData", e))?;
    Ok(read as usize)
}

fn check_cancel(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::SeqCst) {
        Err("cancelled".into())
    } else {
        Ok(())
    }
}

fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.as_os_str().to_owned();
    name.push(".part");
    PathBuf::from(name)
}

/// Downloads `url` into `dest` through `<dest>.part`, renamed on success.
///
/// `progress(done, total)` runs after each chunk; `total` is 0 when the server sent no length.
/// Setting `cancel` stops the transfer, removes the partial file and returns `Err("cancelled")`.
/// Any non-200 status is an error.
pub fn download_to_file(
    url: &str,
    dest: &Path,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64, u64),
) -> Result<(), String> {
    check_cancel(cancel)?;
    let (transfer, status) = send(url, "GET")?;
    if status != 200 {
        return Err(format!("HTTP {status} for {url}"));
    }
    let total = content_length(&transfer);
    let part = part_path(dest);
    let mut file = File::create(&part).map_err(|e| format!("could not create {}: {e}", part.display()))?;
    if let Err(e) = copy_body(&transfer, &mut file, cancel, total, &mut progress) {
        drop(file);
        let _ = fs::remove_file(&part);
        return Err(e);
    }
    drop(file);
    fs::rename(&part, dest).map_err(|e| format!("could not move download to {}: {e}", dest.display()))
}

fn copy_body(
    t: &Transfer,
    out: &mut File,
    cancel: &AtomicBool,
    total: u64,
    progress: &mut impl FnMut(u64, u64),
) -> Result<(), String> {
    let mut buf = vec![0u8; CHUNK];
    let mut done = 0u64;
    loop {
        check_cancel(cancel)?;
        let n = read_chunk(t, &mut buf)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n]).map_err(|e| format!("write failed: {e}"))?;
        done += n as u64;
        progress(done, total);
    }
    if total != 0 && done != total {
        return Err(format!("download ended after {done} of {total} bytes"));
    }
    Ok(())
}

/// GET `url` and return its body as text. Fails when the body exceeds `max_bytes` or the status is not 200.
pub fn fetch_text(url: &str, max_bytes: usize) -> Result<String, String> {
    let (transfer, status) = send(url, "GET")?;
    if status != 200 {
        return Err(format!("HTTP {status} for {url}"));
    }
    let mut body = Vec::new();
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = read_chunk(&transfer, &mut buf)?;
        if n == 0 {
            break;
        }
        if body.len() + n > max_bytes {
            return Err(format!("response from {url} is larger than {max_bytes} bytes"));
        }
        body.extend_from_slice(&buf[..n]);
    }
    Ok(String::from_utf8_lossy(&body).into_owned())
}

/// HEAD `url` and return the status code (200 = present, 404 = absent). Redirects are followed.
pub fn head_status(url: &str) -> Result<u32, String> {
    let (_transfer, status) = send(url, "HEAD")?;
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::{TcpListener, TcpStream};
    use std::thread;

    const BIG: usize = 300 * 1024;

    fn pattern() -> Vec<u8> {
        (0..BIG).map(|i| (i % 251) as u8).collect()
    }

    /// Starts a minimal HTTP/1.1 server on 127.0.0.1 and returns its base URL.
    fn serve() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        let redirect_target = format!("{base}/big");
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let target = redirect_target.clone();
                thread::spawn(move || {
                    let mut stream = stream;
                    let _ = handle(&mut stream, &target);
                });
            }
        });
        base
    }

    fn handle(stream: &mut TcpStream, redirect: &str) -> std::io::Result<()> {
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut request_line = String::new();
        reader.read_line(&mut request_line)?;
        loop {
            let mut header = String::new();
            if reader.read_line(&mut header)? == 0 || header == "\r\n" {
                break;
            }
        }
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or_default().to_string();
        let path = parts.next().unwrap_or_default().to_string();

        let (status, location, body): (&str, Option<&str>, Vec<u8>) = match (method.as_str(), path.as_str()) {
            ("GET" | "HEAD", "/big") => ("200 OK", None, pattern()),
            ("GET" | "HEAD", "/text") => ("200 OK", None, b"hello".to_vec()),
            ("GET" | "HEAD", "/redirect") => ("302 Found", Some(redirect), Vec::new()),
            _ => ("404 Not Found", None, Vec::new()),
        };
        let location_header = location.map(|l| format!("Location: {l}\r\n")).unwrap_or_default();
        let head = format!(
            "HTTP/1.1 {status}\r\n{location_header}Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(head.as_bytes())?;
        if method != "HEAD" {
            stream.write_all(&body)?;
        }
        stream.flush()
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tmop-http-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    #[test]
    fn download_writes_file_and_reports_progress() {
        let base = serve();
        let dir = scratch("progress");
        let dest = dir.join("big.bin");
        let cancel = AtomicBool::new(false);
        let mut calls = Vec::new();
        download_to_file(&format!("{base}/big"), &dest, &cancel, |done, total| calls.push((done, total))).expect("download");
        assert_eq!(fs::read(&dest).expect("read"), pattern());
        assert!(calls.len() >= 2, "expected several chunks, got {calls:?}");
        assert_eq!(calls.last(), Some(&(BIG as u64, BIG as u64)));
        assert!(!part_path(&dest).exists());
    }

    #[test]
    fn download_follows_redirects() {
        let base = serve();
        let dir = scratch("redirect");
        let dest = dir.join("redirected.bin");
        let cancel = AtomicBool::new(false);
        download_to_file(&format!("{base}/redirect"), &dest, &cancel, |_, _| {}).expect("download");
        assert_eq!(fs::read(&dest).expect("read"), pattern());
    }

    #[test]
    fn download_404_is_error_and_leaves_nothing() {
        let base = serve();
        let dir = scratch("missing");
        let dest = dir.join("missing.bin");
        let cancel = AtomicBool::new(false);
        let err = download_to_file(&format!("{base}/missing"), &dest, &cancel, |_, _| {}).unwrap_err();
        assert!(err.contains("HTTP 404"), "{err}");
        assert!(!dest.exists());
        assert!(!part_path(&dest).exists());
    }

    #[test]
    fn cancel_before_start_leaves_nothing() {
        let base = serve();
        let dir = scratch("cancel-before");
        let dest = dir.join("never.bin");
        let cancel = AtomicBool::new(true);
        let err = download_to_file(&format!("{base}/big"), &dest, &cancel, |_, _| {}).unwrap_err();
        assert_eq!(err, "cancelled");
        assert!(!dest.exists());
        assert!(!part_path(&dest).exists());
    }

    #[test]
    fn cancel_during_transfer_removes_partial_file() {
        let base = serve();
        let dir = scratch("cancel-during");
        let dest = dir.join("partial.bin");
        let cancel = AtomicBool::new(false);
        let err = download_to_file(&format!("{base}/big"), &dest, &cancel, |done, _| {
            if done > 0 {
                cancel.store(true, Ordering::SeqCst);
            }
        })
        .unwrap_err();
        assert_eq!(err, "cancelled");
        assert!(!dest.exists());
        assert!(!part_path(&dest).exists());
    }

    #[test]
    fn fetch_text_returns_body_and_enforces_limit() {
        let base = serve();
        assert_eq!(fetch_text(&format!("{base}/text"), 1024).expect("fetch"), "hello");
        assert!(fetch_text(&format!("{base}/text"), 3).is_err());
        assert!(fetch_text(&format!("{base}/missing"), 1024).unwrap_err().contains("HTTP 404"));
    }

    #[test]
    fn head_status_reports_presence() {
        let base = serve();
        assert_eq!(head_status(&format!("{base}/big")).expect("head"), 200);
        assert_eq!(head_status(&format!("{base}/missing")).expect("head"), 404);
        assert_eq!(head_status(&format!("{base}/redirect")).expect("head"), 200);
    }

    #[test]
    fn crack_splits_url_parts() {
        let (host, port, path, secure) = crack("https://example.test/a/b.tar.gz?x=1").expect("crack");
        assert_eq!((host.as_str(), port, path.as_str(), secure), ("example.test", 443, "/a/b.tar.gz?x=1", true));
        let (_, port, path, secure) = crack("http://127.0.0.1:8080").expect("crack");
        assert_eq!((port, path.as_str(), secure), (8080, "/", false));
        assert!(crack("ftp://example.test/x").is_err());
    }
}
