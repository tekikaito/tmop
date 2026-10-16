//! Per-connection HTTP/1.1 relay between a client and the engine socket.
//!
//! The response direction is a plain byte copy. The request direction is framed so that
//! create bodies can be rewritten and `Upgrade` requests switch to raw bytes.

use std::io;

use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
};

use crate::engine::rewrite::{needs_rewrite, rewrite_create_body, rewrite_volume_body, RewriteKind};

/// Largest request head, or single line of a chunked body, in bytes.
const MAX_HEAD: usize = 64 * 1024;
/// Content-Length bodies up to this size are buffered so they can be rewritten; larger ones stream unchanged.
const MAX_BUFFERED_BODY: u64 = 8 * 1024 * 1024;

/// Body rewriting hook for requests.
pub trait Rewriter: Send + Sync {
    /// Returns a replacement body, or `None` to forward the original.
    fn rewrite(&self, method: &str, path: &str, body: &[u8]) -> Option<Vec<u8>>;
}

/// Rewrites Windows bind-mount sources in container and volume create requests.
pub struct MountRewriter;

impl Rewriter for MountRewriter {
    fn rewrite(&self, method: &str, path: &str, body: &[u8]) -> Option<Vec<u8>> {
        match needs_rewrite(method, path) {
            RewriteKind::ContainerCreate => rewrite_create_body(body),
            RewriteKind::VolumeCreate => rewrite_volume_body(body),
            RewriteKind::None => None,
        }
    }
}

/// A parsed request head.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Head {
    pub method: String,
    pub path: String,
    /// Header name and value (trimmed), in order.
    pub headers: Vec<(String, String)>,
    pub content_length: Option<u64>,
    pub chunked: bool,
    pub upgrade: bool,
}

/// Relays one client connection to one upstream connection until both directions finish.
pub async fn relay_connection<C, U>(
    client: C,
    upstream: U,
    rewriter: &dyn Rewriter,
    log: &dyn Fn(&str),
) -> io::Result<()>
where
    C: AsyncRead + AsyncWrite + Unpin,
    U: AsyncRead + AsyncWrite + Unpin,
{
    let (client_r, client_w) = tokio::io::split(client);
    let (upstream_r, upstream_w) = tokio::io::split(upstream);
    // An error in either direction drops the other future.
    tokio::try_join!(
        copy_response(upstream_r, client_w),
        copy_requests(BufReader::new(client_r), upstream_w, rewriter, log),
    )?;
    Ok(())
}

async fn copy_response<R, W>(mut upstream: R, mut client: W) -> io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    tokio::io::copy(&mut upstream, &mut client).await?;
    let _ = client.shutdown().await;
    Ok(())
}

async fn copy_requests<R, W>(
    mut client: R,
    mut upstream: W,
    rewriter: &dyn Rewriter,
    log: &dyn Fn(&str),
) -> io::Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    while let Some(raw) = read_head(&mut client).await? {
        let head = parse_head(&raw).ok_or_else(|| invalid("malformed request head"))?;
        if head.chunked {
            // Only rewrite candidates are buffered; other chunked bodies stream through as they are.
            if matches!(needs_rewrite(&head.method, &head.path), RewriteKind::None) {
                upstream.write_all(&raw).await?;
                copy_chunked(&mut client, &mut upstream).await?;
            } else {
                forward_chunked(&mut client, &mut upstream, &raw, &head, MAX_BUFFERED_BODY, rewriter, log).await?;
            }
        } else if let Some(len) = head.content_length {
            forward_sized(&mut client, &mut upstream, &raw, &head, len, rewriter, log).await?;
        } else {
            upstream.write_all(&raw).await?;
        }
        upstream.flush().await?;
        if head.upgrade {
            log(&format!("{} {} switched to raw passthrough", head.method, head.path));
            tokio::io::copy(&mut client, &mut upstream).await?;
            break;
        }
    }
    let _ = upstream.shutdown().await;
    Ok(())
}

/// Forwards a Content-Length body, rewriting it when the rewriter asks for it.
async fn forward_sized<R, W>(
    client: &mut R,
    upstream: &mut W,
    raw: &[u8],
    head: &Head,
    len: u64,
    rewriter: &dyn Rewriter,
    log: &dyn Fn(&str),
) -> io::Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    if len > MAX_BUFFERED_BODY {
        upstream.write_all(raw).await?;
        let copied = tokio::io::copy(&mut (&mut *client).take(len), upstream).await?;
        if copied != len {
            return Err(eof("client closed inside request body"));
        }
        return Ok(());
    }
    let mut body = vec![0u8; len as usize];
    client.read_exact(&mut body).await?;
    match rewriter.rewrite(&head.method, &head.path, &body) {
        Some(new) => {
            log(&format!("rewrote request body of {} {}", head.method, head.path));
            upstream.write_all(&render_head(head, Some(new.len() as u64))).await?;
            upstream.write_all(&new).await?;
        }
        None => {
            upstream.write_all(raw).await?;
            upstream.write_all(&body).await?;
        }
    }
    Ok(())
}

/// Copies a chunked body verbatim, up to and including the trailer section.
async fn copy_chunked<R, W>(client: &mut R, upstream: &mut W) -> io::Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    loop {
        let mut line = Vec::new();
        if read_line_limited(client, &mut line, MAX_HEAD).await? == 0 {
            return Err(eof("client closed inside chunked body"));
        }
        upstream.write_all(&line).await?;
        let size = chunk_size(&line).ok_or_else(|| invalid("malformed chunk size"))?;
        if size == 0 {
            loop {
                let mut trailer = Vec::new();
                if read_line_limited(client, &mut trailer, MAX_HEAD).await? == 0 {
                    return Err(eof("client closed inside chunked trailer"));
                }
                upstream.write_all(&trailer).await?;
                if trailer == b"\r\n" || trailer == b"\n" {
                    return Ok(());
                }
            }
        }
        copy_chunk_data(client, upstream, size).await?;
    }
}

/// Copies one chunk's data and the CRLF after it.
async fn copy_chunk_data<R, W>(client: &mut R, upstream: &mut W, size: u64) -> io::Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let total = chunk_total(size)?;
    let copied = tokio::io::copy(&mut (&mut *client).take(total), upstream).await?;
    if copied != total {
        return Err(eof("client closed inside chunk data"));
    }
    Ok(())
}

/// Size of a chunk's data plus its CRLF. A size that cannot be represented is invalid data.
fn chunk_total(size: u64) -> io::Result<u64> {
    size.checked_add(2).ok_or_else(|| invalid("chunk size overflows"))
}

/// Outcome of [`buffer_chunked`].
enum Buffered {
    /// The whole body was read: `wire` holds it as sent, `body` the decoded data.
    Complete,
    /// The decoded data would pass the limit at the chunk of this size. `wire` holds the bytes read
    /// before it and `line` its size line; the chunk data is still unread.
    Overflow { size: u64, line: Vec<u8> },
}

/// Forwards a chunked body on a rewrite-candidate path. The body is buffered (up to `limit` decoded
/// bytes) and rewritten, then sent with a Content-Length. A larger body is forwarded unchanged.
async fn forward_chunked<R, W>(
    client: &mut R,
    upstream: &mut W,
    raw: &[u8],
    head: &Head,
    limit: u64,
    rewriter: &dyn Rewriter,
    log: &dyn Fn(&str),
) -> io::Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut wire = Vec::new();
    let mut body = Vec::new();
    match buffer_chunked(client, &mut wire, &mut body, limit).await? {
        Buffered::Complete => match rewriter.rewrite(&head.method, &head.path, &body) {
            Some(new) => {
                log(&format!("rewrote request body of {} {}", head.method, head.path));
                upstream.write_all(&render_head(head, Some(new.len() as u64))).await?;
                upstream.write_all(&new).await?;
            }
            None => {
                upstream.write_all(raw).await?;
                upstream.write_all(&wire).await?;
            }
        },
        Buffered::Overflow { size, line } => {
            upstream.write_all(raw).await?;
            upstream.write_all(&wire).await?;
            upstream.write_all(&line).await?;
            copy_chunk_data(client, upstream, size).await?;
            copy_chunked(client, upstream).await?;
        }
    }
    Ok(())
}

/// Reads a chunked body into `wire` (bytes as sent) and `body` (decoded data), stopping once the
/// decoded data would pass `limit`.
async fn buffer_chunked<R: AsyncBufRead + Unpin>(client: &mut R, wire: &mut Vec<u8>, body: &mut Vec<u8>, limit: u64) -> io::Result<Buffered> {
    loop {
        let mut line = Vec::new();
        if read_line_limited(client, &mut line, MAX_HEAD).await? == 0 {
            return Err(eof("client closed inside chunked body"));
        }
        let size = chunk_size(&line).ok_or_else(|| invalid("malformed chunk size"))?;
        if size == 0 {
            wire.extend_from_slice(&line);
            read_trailer(client, wire).await?;
            return Ok(Buffered::Complete);
        }
        if (body.len() as u64).saturating_add(size) > limit {
            return Ok(Buffered::Overflow { size, line });
        }
        wire.extend_from_slice(&line);
        let total = chunk_total(size)?;
        let start = wire.len();
        let read = (&mut *client).take(total).read_to_end(wire).await?;
        if read as u64 != total {
            return Err(eof("client closed inside chunk data"));
        }
        body.extend_from_slice(&wire[start..start + size as usize]);
    }
}

/// Appends trailer lines to `out`, through the blank line that ends a chunked body.
async fn read_trailer<R: AsyncBufRead + Unpin>(client: &mut R, out: &mut Vec<u8>) -> io::Result<()> {
    loop {
        let start = out.len();
        if read_line_limited(client, out, MAX_HEAD).await? == 0 {
            return Err(eof("client closed inside chunked trailer"));
        }
        let line = &out[start..];
        if line == b"\r\n" || line == b"\n" {
            return Ok(());
        }
    }
}

/// Reads a request head through its blank line. `None` on a clean EOF before any bytes.
async fn read_head<R: AsyncBufRead + Unpin>(client: &mut R) -> io::Result<Option<Vec<u8>>> {
    let mut head = Vec::new();
    loop {
        let start = head.len();
        if read_line_limited(client, &mut head, MAX_HEAD).await? == 0 {
            return if head.is_empty() {
                Ok(None)
            } else {
                Err(eof("client closed inside request head"))
            };
        }
        let line = &head[start..];
        if line == b"\r\n" || line == b"\n" {
            // stray blank lines before a request line are skipped
            if start == 0 {
                head.clear();
                continue;
            }
            return Ok(Some(head));
        }
    }
}

/// Appends bytes through the next `\n` (inclusive) to `out`. Returns the bytes read, 0 only at EOF.
///
/// Fails once `out` grows past `max`.
async fn read_line_limited<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    out: &mut Vec<u8>,
    max: usize,
) -> io::Result<usize> {
    let mut total = 0;
    loop {
        let (used, done) = {
            let buf = reader.fill_buf().await?;
            if buf.is_empty() {
                return Ok(total);
            }
            match buf.iter().position(|&b| b == b'\n') {
                Some(i) => {
                    out.extend_from_slice(&buf[..=i]);
                    (i + 1, true)
                }
                None => {
                    out.extend_from_slice(buf);
                    (buf.len(), false)
                }
            }
        };
        reader.consume(used);
        total += used;
        if out.len() > max {
            return Err(invalid("request head or line exceeds limit"));
        }
        if done {
            return Ok(total);
        }
    }
}

fn chunk_size(line: &[u8]) -> Option<u64> {
    let text = std::str::from_utf8(line).ok()?.trim();
    let hex = text.split(';').next()?.trim();
    if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u64::from_str_radix(hex, 16).ok()
}

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

fn eof(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::UnexpectedEof, msg)
}

/// Parses a request head (with or without its terminating blank line).
pub fn parse_head(head: &[u8]) -> Option<Head> {
    let text = std::str::from_utf8(head).ok()?;
    let mut lines = text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l));
    let mut request = lines.next()?.split(' ');
    let method = request.next().filter(|m| !m.is_empty())?;
    let path = request.next().filter(|p| !p.is_empty())?;
    let version = request.next()?;
    if !version.starts_with("HTTP/") || request.next().is_some() {
        return None;
    }

    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        let (name, value) = line.split_once(':')?;
        let name = name.trim();
        if name.is_empty() {
            return None;
        }
        headers.push((name.to_string(), value.trim().to_string()));
    }

    let mut content_length = None;
    let mut chunked = false;
    let mut upgrade = false;
    for (name, value) in &headers {
        if name.eq_ignore_ascii_case("content-length") {
            if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let len = value.parse::<u64>().ok()?;
            // Repeated Content-Length headers are accepted only when they agree.
            if content_length.is_some_and(|prev| prev != len) {
                return None;
            }
            content_length = Some(len);
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            chunked |= value.split(',').any(|t| t.trim().eq_ignore_ascii_case("chunked"));
        } else if name.eq_ignore_ascii_case("connection") {
            upgrade |= value.split(',').any(|t| t.trim().eq_ignore_ascii_case("upgrade"));
        } else if name.eq_ignore_ascii_case("upgrade") {
            upgrade = true;
        }
    }

    Some(Head {
        method: method.to_string(),
        path: path.to_string(),
        headers,
        content_length,
        chunked,
        upgrade,
    })
}

/// Renders an HTTP/1.1 request head with `Content-Length` set to `content_length`.
///
/// `None` keeps the headers as they were. `Some(n)` replaces the first Content-Length in place
/// (or appends one) and drops any other. For a chunked head it also drops Transfer-Encoding.
pub fn render_head(head: &Head, content_length: Option<u64>) -> Vec<u8> {
    let mut out = format!("{} {} HTTP/1.1\r\n", head.method, head.path);
    let mut wrote_length = false;
    for (name, value) in &head.headers {
        if let Some(len) = content_length {
            let framing = name.eq_ignore_ascii_case("content-length") || (head.chunked && name.eq_ignore_ascii_case("transfer-encoding"));
            if framing {
                if !wrote_length {
                    out.push_str(&format!("Content-Length: {len}\r\n"));
                    wrote_length = true;
                }
                continue;
            }
        }
        out.push_str(&format!("{name}: {value}\r\n"));
    }
    if let Some(len) = content_length {
        if !wrote_length {
            out.push_str(&format!("Content-Length: {len}\r\n"));
        }
    }
    out.push_str("\r\n");
    out.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    const GET: &[u8] = b"GET /version HTTP/1.1\r\nHost: docker\r\n\r\n";
    const RESP: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
    const TEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

    #[test]
    fn parses_request_head() {
        let h = parse_head(b"POST /v1.47/containers/create?name=x HTTP/1.1\r\nHost: a\r\nContent-Length: 12\r\nTransfer-Encoding: gzip, chunked\r\n\r\n")
            .unwrap();
        assert_eq!(h.method, "POST");
        assert_eq!(h.path, "/v1.47/containers/create?name=x");
        assert_eq!(h.headers.len(), 3);
        assert_eq!(h.content_length, Some(12));
        assert!(h.chunked);
        assert!(!h.upgrade);
    }

    #[test]
    fn detects_upgrade_tokens() {
        let h = parse_head(b"POST /x HTTP/1.1\r\nConnection: keep-alive, Upgrade\r\n\r\n").unwrap();
        assert!(h.upgrade);
        let h = parse_head(b"POST /x HTTP/1.1\r\nupgrade: tcp\r\n\r\n").unwrap();
        assert!(h.upgrade);
        let h = parse_head(b"GET /x HTTP/1.1\r\nConnection: close\r\n\r\n").unwrap();
        assert!(!h.upgrade);
    }

    #[test]
    fn rejects_malformed_heads() {
        assert_eq!(parse_head(b"garbage\r\n\r\n"), None);
        assert_eq!(parse_head(b"GET / HTTP/1.1 extra\r\n\r\n"), None);
        assert_eq!(parse_head(b"GET / HTTP/1.1\r\nContent-Length: x\r\n\r\n"), None);
        assert_eq!(parse_head(b"GET / HTTP/1.1\r\nno colon here\r\n\r\n"), None);
    }

    #[test]
    fn render_replaces_content_length_in_place() {
        let h = parse_head(b"POST /x HTTP/1.1\r\nHost: a\r\nContent-Length: 12\r\nX-Y: z\r\n\r\n").unwrap();
        assert_eq!(
            render_head(&h, Some(5)),
            b"POST /x HTTP/1.1\r\nHost: a\r\nContent-Length: 5\r\nX-Y: z\r\n\r\n"
        );
        assert_eq!(
            render_head(&h, None),
            b"POST /x HTTP/1.1\r\nHost: a\r\nContent-Length: 12\r\nX-Y: z\r\n\r\n"
        );
        let no_len = parse_head(b"POST /x HTTP/1.1\r\nHost: a\r\n\r\n").unwrap();
        assert_eq!(render_head(&no_len, Some(3)), b"POST /x HTTP/1.1\r\nHost: a\r\nContent-Length: 3\r\n\r\n");
    }

    #[tokio::test]
    async fn get_passes_through_unchanged() {
        let (mut client, relay_client) = duplex(1 << 16);
        let (relay_up, mut fake_up) = duplex(1 << 16);
        let rewriter = MountRewriter;
        let log = |_: &str| {};
        let fake = async move {
            let mut got = vec![0u8; GET.len()];
            fake_up.read_exact(&mut got).await.unwrap();
            assert_eq!(got, GET);
            fake_up.write_all(RESP).await.unwrap();
        };
        let client_fut = async {
            client.write_all(GET).await.unwrap();
            let mut resp = vec![0u8; RESP.len()];
            client.read_exact(&mut resp).await.unwrap();
            assert_eq!(resp, RESP);
            drop(client);
        };
        let (res, _, _) = tokio::time::timeout(TEST_TIMEOUT, async { tokio::join!(relay_connection(relay_client, relay_up, &rewriter, &log), client_fut, fake) }).await.expect("relay test timed out");
        res.unwrap();
    }

    #[tokio::test]
    async fn client_eof_shuts_upstream_write_half() {
        let (mut client, relay_client) = duplex(1 << 16);
        let (relay_up, mut fake_up) = duplex(1 << 16);
        let rewriter = MountRewriter;
        let log = |_: &str| {};
        let fake = async move {
            let mut got = Vec::new();
            fake_up.read_to_end(&mut got).await.unwrap();
            assert_eq!(got, GET);
        };
        let client_fut = async {
            client.write_all(GET).await.unwrap();
            drop(client);
        };
        let (res, _, _) = tokio::time::timeout(TEST_TIMEOUT, async { tokio::join!(relay_connection(relay_client, relay_up, &rewriter, &log), client_fut, fake) }).await.expect("relay test timed out");
        res.unwrap();
    }

    #[tokio::test]
    async fn container_create_bind_is_rewritten() {
        let body = r#"{"Image":"alpine","HostConfig":{"Binds":["C:\\Users\\me\\proj:/app:ro"]}}"#;
        let req = format!(
            "POST /v1.47/containers/create?name=x HTTP/1.1\r\nHost: docker\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let (mut client, relay_client) = duplex(1 << 16);
        let (relay_up, fake_up) = duplex(1 << 16);
        let rewriter = MountRewriter;
        let log = |_: &str| {};
        let fake = async move {
            let (fr, mut fw) = tokio::io::split(fake_up);
            let mut fr = BufReader::new(fr);
            let raw = read_head(&mut fr).await.unwrap().unwrap();
            let head = parse_head(&raw).unwrap();
            let len = head.content_length.unwrap();
            let mut got = vec![0u8; len as usize];
            fr.read_exact(&mut got).await.unwrap();
            let text = String::from_utf8(got).unwrap();
            assert!(text.contains(r#""/mnt/c/Users/me/proj:/app:ro""#), "{text}");
            assert_eq!(len, text.len() as u64);
            fw.write_all(RESP).await.unwrap();
            let mut rest = Vec::new();
            fr.read_to_end(&mut rest).await.unwrap();
            assert!(rest.is_empty());
        };
        let client_fut = async {
            client.write_all(req.as_bytes()).await.unwrap();
            let mut resp = vec![0u8; RESP.len()];
            client.read_exact(&mut resp).await.unwrap();
            assert_eq!(resp, RESP);
            drop(client);
        };
        let (res, _, _) = tokio::time::timeout(TEST_TIMEOUT, async { tokio::join!(relay_connection(relay_client, relay_up, &rewriter, &log), client_fut, fake) }).await.expect("relay test timed out");
        res.unwrap();
    }

    #[tokio::test]
    async fn chunked_post_is_forwarded_byte_exact() {
        let req: &[u8] = b"POST /containers/x/exec HTTP/1.1\r\nHost: docker\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n3;ext=1\r\nabc\r\n0\r\nX-Trailer: 1\r\n\r\n";
        let (mut client, relay_client) = duplex(1 << 16);
        let (relay_up, mut fake_up) = duplex(1 << 16);
        let rewriter = MountRewriter;
        let log = |_: &str| {};
        let fake = async move {
            let mut got = vec![0u8; req.len()];
            fake_up.read_exact(&mut got).await.unwrap();
            assert_eq!(got, req);
            fake_up.write_all(RESP).await.unwrap();
            let mut rest = Vec::new();
            fake_up.read_to_end(&mut rest).await.unwrap();
            assert!(rest.is_empty());
        };
        let client_fut = async {
            client.write_all(req).await.unwrap();
            let mut resp = vec![0u8; RESP.len()];
            client.read_exact(&mut resp).await.unwrap();
            assert_eq!(resp, RESP);
            drop(client);
        };
        let (res, _, _) = tokio::time::timeout(TEST_TIMEOUT, async { tokio::join!(relay_connection(relay_client, relay_up, &rewriter, &log), client_fut, fake) }).await.expect("relay test timed out");
        res.unwrap();
    }

    #[tokio::test]
    async fn upgrade_switches_to_raw_passthrough() {
        let req: &[u8] = b"POST /containers/x/attach HTTP/1.1\r\nHost: docker\r\nConnection: Upgrade\r\nUpgrade: tcp\r\n\r\n";
        let switching: &[u8] = b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: tcp\r\n\r\n";
        let (mut client, relay_client) = duplex(1 << 16);
        let (relay_up, mut fake_up) = duplex(1 << 16);
        let rewriter = MountRewriter;
        let log = |_: &str| {};
        let fake = async move {
            let mut got = vec![0u8; req.len()];
            fake_up.read_exact(&mut got).await.unwrap();
            assert_eq!(got, req);
            fake_up.write_all(switching).await.unwrap();
            let mut raw = [0u8; 10];
            fake_up.read_exact(&mut raw).await.unwrap();
            assert_eq!(&raw, b"client-raw");
            fake_up.write_all(b"server-raw").await.unwrap();
            let mut rest = Vec::new();
            fake_up.read_to_end(&mut rest).await.unwrap();
            assert!(rest.is_empty());
        };
        let client_fut = async {
            client.write_all(req).await.unwrap();
            let mut resp = vec![0u8; switching.len()];
            client.read_exact(&mut resp).await.unwrap();
            assert_eq!(resp, switching);
            client.write_all(b"client-raw").await.unwrap();
            let mut raw = [0u8; 10];
            client.read_exact(&mut raw).await.unwrap();
            assert_eq!(&raw, b"server-raw");
            drop(client);
        };
        let (res, _, _) = tokio::time::timeout(TEST_TIMEOUT, async { tokio::join!(relay_connection(relay_client, relay_up, &rewriter, &log), client_fut, fake) }).await.expect("relay test timed out");
        res.unwrap();
    }

    #[tokio::test]
    async fn two_keep_alive_requests_on_one_connection() {
        let (mut client, relay_client) = duplex(1 << 16);
        let (relay_up, mut fake_up) = duplex(1 << 16);
        let rewriter = MountRewriter;
        let log = |_: &str| {};
        let fake = async move {
            for _ in 0..2 {
                let mut got = vec![0u8; GET.len()];
                fake_up.read_exact(&mut got).await.unwrap();
                assert_eq!(got, GET);
                fake_up.write_all(RESP).await.unwrap();
            }
            let mut rest = Vec::new();
            fake_up.read_to_end(&mut rest).await.unwrap();
            assert!(rest.is_empty());
        };
        let client_fut = async {
            for _ in 0..2 {
                client.write_all(GET).await.unwrap();
                let mut resp = vec![0u8; RESP.len()];
                client.read_exact(&mut resp).await.unwrap();
                assert_eq!(resp, RESP);
            }
            drop(client);
        };
        let (res, _, _) = tokio::time::timeout(TEST_TIMEOUT, async { tokio::join!(relay_connection(relay_client, relay_up, &rewriter, &log), client_fut, fake) }).await.expect("relay test timed out");
        res.unwrap();
    }

    #[tokio::test]
    async fn oversized_head_is_an_error() {
        let (mut client, relay_client) = duplex(1 << 17);
        let (relay_up, _fake_up) = duplex(1 << 16);
        let rewriter = MountRewriter;
        let log = |_: &str| {};
        let big = vec![b'A'; MAX_HEAD + 1024];
        client.write_all(&big).await.unwrap();
        let res = relay_connection(relay_client, relay_up, &rewriter, &log).await;
        assert!(res.is_err());
    }

    #[test]
    fn conflicting_content_length_is_rejected() {
        assert_eq!(parse_head(b"POST /x HTTP/1.1\r\nContent-Length: 5\r\nContent-Length: 6\r\n\r\n"), None);
        let h = parse_head(b"POST /x HTTP/1.1\r\nContent-Length: 5\r\ncontent-length: 5\r\n\r\n").unwrap();
        assert_eq!(h.content_length, Some(5));
    }

    #[tokio::test]
    async fn chunked_container_create_is_rewritten_with_content_length() {
        let body = r#"{"Image":"alpine","HostConfig":{"Binds":["C:\\Users\\me\\proj:/app:ro"]}}"#;
        let (a, b) = body.split_at(20);
        let req = format!(
            "POST /v1.47/containers/create?name=x HTTP/1.1\r\nHost: docker\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{a}\r\n{:x}\r\n{b}\r\n0\r\n\r\n",
            a.len(),
            b.len()
        );
        let (mut client, relay_client) = duplex(1 << 16);
        let (relay_up, fake_up) = duplex(1 << 16);
        let rewriter = MountRewriter;
        let log = |_: &str| {};
        let fake = async move {
            let (fr, mut fw) = tokio::io::split(fake_up);
            let mut fr = BufReader::new(fr);
            let raw = read_head(&mut fr).await.unwrap().unwrap();
            assert!(!String::from_utf8_lossy(&raw).to_ascii_lowercase().contains("transfer-encoding"));
            let head = parse_head(&raw).unwrap();
            assert!(!head.chunked);
            let len = head.content_length.unwrap();
            let mut got = vec![0u8; len as usize];
            fr.read_exact(&mut got).await.unwrap();
            let text = String::from_utf8(got).unwrap();
            assert!(text.contains(r#""/mnt/c/Users/me/proj:/app:ro""#), "{text}");
            assert_eq!(len, text.len() as u64);
            fw.write_all(RESP).await.unwrap();
            let mut rest = Vec::new();
            fr.read_to_end(&mut rest).await.unwrap();
            assert!(rest.is_empty());
        };
        let client_fut = async {
            client.write_all(req.as_bytes()).await.unwrap();
            let mut resp = vec![0u8; RESP.len()];
            client.read_exact(&mut resp).await.unwrap();
            assert_eq!(resp, RESP);
            drop(client);
        };
        let (res, _, _) = tokio::time::timeout(TEST_TIMEOUT, async { tokio::join!(relay_connection(relay_client, relay_up, &rewriter, &log), client_fut, fake) }).await.expect("relay test timed out");
        res.unwrap();
    }

    #[tokio::test]
    async fn chunked_candidate_body_is_forwarded_unchanged_when_not_rewritten_or_too_big() {
        let raw: &[u8] = b"POST /containers/create HTTP/1.1\r\nHost: docker\r\nTransfer-Encoding: chunked\r\n\r\n";
        let head = parse_head(raw).unwrap();
        let log = |_: &str| {};
        // Over the limit: the first chunk is buffered, the second streams through.
        let rest: &[u8] = b"3\r\nabc\r\n4\r\ndefg\r\n0\r\n\r\n";
        let mut up = Vec::new();
        forward_chunked(&mut BufReader::new(rest), &mut up, raw, &head, 4, &MountRewriter, &log).await.unwrap();
        assert_eq!(up, [raw, rest].concat());
        // Within the limit but not a JSON create body: the rewriter declines, so the bytes are kept.
        let rest: &[u8] = b"3\r\nabc\r\n0\r\n\r\n";
        let mut up = Vec::new();
        forward_chunked(&mut BufReader::new(rest), &mut up, raw, &head, 1024, &MountRewriter, &log).await.unwrap();
        assert_eq!(up, [raw, rest].concat());
    }

    #[tokio::test]
    async fn chunk_size_overflow_is_an_error() {
        // The plain path and the rewrite-candidate path both reject the size without panicking.
        for path in ["/x", "/containers/create"] {
            let req = format!("POST {path} HTTP/1.1\r\nHost: docker\r\nTransfer-Encoding: chunked\r\n\r\nffffffffffffffff\r\n");
            let (mut client, relay_client) = duplex(1 << 16);
            let (relay_up, _fake_up) = duplex(1 << 16);
            client.write_all(req.as_bytes()).await.unwrap();
            let log = |_: &str| {};
            let res = tokio::time::timeout(TEST_TIMEOUT, relay_connection(relay_client, relay_up, &MountRewriter, &log)).await.expect("relay test timed out");
            assert_eq!(res.unwrap_err().kind(), io::ErrorKind::InvalidData, "{path}");
        }
    }
}
