//! A local HTTP endpoint the video player streams clips from.
//!
//! WebKit on macOS does not decode media in the web process: it hands the
//! element's URL to AVFoundation, which opens it itself. AVFoundation knows
//! nothing of Tauri's `asset:` scheme, and it cannot read a page's `blob:`
//! URLs either — both end in a sized black box that never plays and never
//! reports an error. What it does read is plain HTTP, including byte-range
//! requests for seeking, so the player is given an address on this computer.
//!
//! The server listens on 127.0.0.1 only, on a port the system picks. It
//! serves only the clips the player registered through `stream_url`, and
//! only to requests carrying the per-session token, so another program on
//! the machine cannot use it to read files. Responses allow any origin, so
//! the player's canvas stays untainted and "Save frame" can read the pixels.

use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::Duration,
};

struct Server {
    port: u16,
    token: String,
    clips: Mutex<Vec<PathBuf>>,
}

static SERVER: OnceLock<Result<Server, String>> = OnceLock::new();

fn server() -> Result<&'static Server, String> {
    SERVER
        .get_or_init(|| {
            let listener = TcpListener::bind(("127.0.0.1", 0))
                .map_err(|e| format!("could not start the video server: {e}"))?;
            let port = listener
                .local_addr()
                .map_err(|e| format!("could not start the video server: {e}"))?
                .port();
            std::thread::Builder::new()
                .name("video-server".into())
                .spawn(move || {
                    for stream in listener.incoming().flatten() {
                        std::thread::spawn(move || {
                            if let Err(e) = serve(stream) {
                                log::debug!("video server connection ended: {e}");
                            }
                        });
                    }
                })
                .map_err(|e| format!("could not start the video server: {e}"))?;
            log::info!("Video server listening on 127.0.0.1:{port}");
            Ok(Server {
                port,
                token: uuid::Uuid::new_v4().simple().to_string(),
                clips: Mutex::new(Vec::new()),
            })
        })
        .as_ref()
        .map_err(|e| e.clone())
}

/// The address the player streams `path` from.
pub fn stream_url(path: &Path) -> Result<String, String> {
    if !crate::formats::is_video_file(path) {
        return Err("That file is not a video this player supports".into());
    }
    if !path.is_file() {
        return Err(format!("{} is not a readable file", path.display()));
    }
    let server = server()?;
    let mut clips = server
        .clips
        .lock()
        .map_err(|_| "video server unavailable")?;
    let id = match clips.iter().position(|p| p == path) {
        Some(i) => i,
        None => {
            clips.push(path.to_path_buf());
            clips.len() - 1
        }
    };
    Ok(format!(
        "http://127.0.0.1:{}/clip/{id}?t={}",
        server.port, server.token
    ))
}

fn content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("mov") => "video/quicktime",
        _ => "video/mp4",
    }
}

/// A single `bytes=` range against a file of `len` bytes, as (start, end)
/// inclusive. `None` for no Range header; `Err` for one that cannot be met.
pub(crate) fn parse_range(header: Option<&str>, len: u64) -> Result<Option<(u64, u64)>, ()> {
    let Some(value) = header else {
        return Ok(None);
    };
    let spec = value.trim().strip_prefix("bytes=").ok_or(())?;
    // One range only; a list is legal but media players never send one.
    let spec = spec.split(',').next().ok_or(())?.trim();
    let (a, b) = spec.split_once('-').ok_or(())?;
    if len == 0 {
        return Err(());
    }
    let (start, end) = match (a.trim(), b.trim()) {
        ("", suffix) => {
            let n: u64 = suffix.parse().map_err(|_| ())?;
            if n == 0 {
                return Err(());
            }
            (len.saturating_sub(n), len - 1)
        }
        (start, "") => (start.parse().map_err(|_| ())?, len - 1),
        (start, end) => {
            let (s, e): (u64, u64) = (start.parse().map_err(|_| ())?, end.parse().map_err(|_| ())?);
            (s, e.min(len - 1))
        }
    };
    if start > end || start >= len {
        return Err(());
    }
    Ok(Some((start, end)))
}

fn respond_empty(out: &mut TcpStream, status: &str) -> std::io::Result<()> {
    write!(
        out,
        "HTTP/1.1 {status}\r\nContent-Length: 0\r\nAccess-Control-Allow-Origin: *\r\n\r\n"
    )
}

fn serve(stream: TcpStream) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(60)))?;
    let mut out = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    // Keep-alive: one connection carries the player's successive range
    // requests until it closes or goes quiet.
    loop {
        let mut request_line = String::new();
        if reader.read_line(&mut request_line)? == 0 {
            return Ok(());
        }
        let mut range = None;
        let mut close = false;
        let mut header_bytes = request_line.len();
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 {
                return Ok(());
            }
            header_bytes += line.len();
            if header_bytes > 16 * 1024 {
                return respond_empty(&mut out, "431 Request Header Fields Too Large");
            }
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                match name.trim().to_ascii_lowercase().as_str() {
                    "range" => range = Some(value.trim().to_string()),
                    "connection" => close = value.trim().eq_ignore_ascii_case("close"),
                    _ => {}
                }
            }
        }
        let mut parts = request_line.split_whitespace();
        let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        answer(&mut out, method, target, range.as_deref())?;
        out.flush()?;
        if close {
            return Ok(());
        }
    }
}

fn answer(
    out: &mut TcpStream,
    method: &str,
    target: &str,
    range: Option<&str>,
) -> std::io::Result<()> {
    if method == "OPTIONS" {
        return write!(
            out,
            "HTTP/1.1 204 No Content\r\nAccess-Control-Allow-Origin: *\r\n\
             Access-Control-Allow-Headers: Range\r\nAccess-Control-Allow-Methods: GET, HEAD\r\n\
             Content-Length: 0\r\n\r\n"
        );
    }
    if method != "GET" && method != "HEAD" {
        return respond_empty(out, "405 Method Not Allowed");
    }
    let Some(server) = SERVER.get().and_then(|s| s.as_ref().ok()) else {
        return respond_empty(out, "503 Service Unavailable");
    };
    let (route, query) = target.split_once('?').unwrap_or((target, ""));
    let token_ok = query
        .split('&')
        .any(|pair| pair.strip_prefix("t=") == Some(server.token.as_str()));
    if !token_ok {
        return respond_empty(out, "403 Forbidden");
    }
    let path = route
        .strip_prefix("/clip/")
        .and_then(|id| id.parse::<usize>().ok())
        .and_then(|id| server.clips.lock().ok()?.get(id).cloned());
    let Some(path) = path else {
        return respond_empty(out, "404 Not Found");
    };
    let Ok(mut file) = File::open(&path) else {
        return respond_empty(out, "404 Not Found");
    };
    let len = file.metadata()?.len();
    let (status, start, end) = match parse_range(range, len) {
        Ok(None) => ("200 OK", 0, len.saturating_sub(1)),
        Ok(Some((s, e))) => ("206 Partial Content", s, e),
        Err(()) => {
            return write!(
                out,
                "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{len}\r\n\
                 Content-Length: 0\r\nAccess-Control-Allow-Origin: *\r\n\r\n"
            );
        }
    };
    let body = if len == 0 { 0 } else { end - start + 1 };
    let mut head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {}\r\nContent-Length: {body}\r\n\
         Accept-Ranges: bytes\r\nAccess-Control-Allow-Origin: *\r\nCache-Control: no-cache\r\n",
        content_type(&path)
    );
    if status.starts_with("206") {
        head.push_str(&format!("Content-Range: bytes {start}-{end}/{len}\r\n"));
    }
    head.push_str("\r\n");
    out.write_all(head.as_bytes())?;
    if method == "HEAD" || body == 0 {
        return Ok(());
    }
    file.seek(SeekFrom::Start(start))?;
    std::io::copy(&mut file.take(body), out)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn ranges_are_read_as_media_players_send_them() {
        assert_eq!(parse_range(None, 100), Ok(None));
        assert_eq!(parse_range(Some("bytes=0-"), 100), Ok(Some((0, 99))));
        assert_eq!(parse_range(Some("bytes=10-19"), 100), Ok(Some((10, 19))));
        assert_eq!(parse_range(Some("bytes=90-500"), 100), Ok(Some((90, 99))));
        assert_eq!(parse_range(Some("bytes=-10"), 100), Ok(Some((90, 99))));
        assert_eq!(parse_range(Some("bytes=0-1"), 100), Ok(Some((0, 1))));
        assert!(parse_range(Some("bytes=100-"), 100).is_err());
        assert!(parse_range(Some("bytes=20-10"), 100).is_err());
        assert!(parse_range(Some("items=0-1"), 100).is_err());
    }

    fn request(url: &str, extra: &str) -> (String, Vec<u8>) {
        let rest = url.strip_prefix("http://").unwrap();
        let (host, path) = rest.split_once('/').unwrap();
        let mut s = TcpStream::connect(host).unwrap();
        write!(
            s,
            "GET /{path} HTTP/1.1\r\nHost: {host}\r\n{extra}Connection: close\r\n\r\n"
        )
        .unwrap();
        let mut all = Vec::new();
        s.read_to_end(&mut all).unwrap();
        let split = all.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        (
            String::from_utf8_lossy(&all[..split]).to_string(),
            all[split + 4..].to_vec(),
        )
    }

    #[test]
    fn serves_registered_clips_with_ranges_and_refuses_everything_else() {
        let dir = tempfile::tempdir().unwrap();
        let clip = dir.path().join("clip 20:32 +0000.MOV");
        let bytes: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&clip, &bytes).unwrap();
        let url = stream_url(&clip).unwrap();
        assert!(url.starts_with("http://127.0.0.1:"));
        assert_eq!(stream_url(&clip).unwrap(), url, "one address per clip");

        let (head, body) = request(&url, "");
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        assert!(head.contains("Content-Type: video/quicktime"));
        assert!(head.contains("Accept-Ranges: bytes"));
        assert!(head.contains("Access-Control-Allow-Origin: *"));
        assert_eq!(body, bytes);

        let (head, body) = request(&url, "Range: bytes=1000-1999\r\n");
        assert!(head.starts_with("HTTP/1.1 206"), "{head}");
        assert!(head.contains("Content-Range: bytes 1000-1999/5000"));
        assert_eq!(body, bytes[1000..2000]);

        let (head, _) = request(&url, "Range: bytes=9000-\r\n");
        assert!(head.starts_with("HTTP/1.1 416"), "{head}");

        let wrong_token = format!("{}x", url);
        assert!(request(&wrong_token, "").0.starts_with("HTTP/1.1 403"));
        let unknown = url.replace("/clip/", "/clip/999");
        assert!(request(&unknown, "").0.starts_with("HTTP/1.1 404"));
        assert!(stream_url(&dir.path().join("photo.jpg")).is_err());
    }
}
