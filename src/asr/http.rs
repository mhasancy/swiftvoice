//! Minimal HTTPS GET used only by the model manager (explicit user action).
//!
//! * Windows: WinHTTP, which uses the system proxy settings and certificate
//!   store, follows redirects (GitHub and Hugging Face both redirect to a
//!   CDN) and needs no extra libraries.
//! * Other platforms (development only): the `curl` command-line tool.
//!
//! Dictation never touches the network; only `models download` and the
//! tray's download items call this module.

use std::io::Read;

use anyhow::{bail, Result};

pub struct HttpResponse {
    pub status: u16,
    pub content_length: Option<u64>,
    pub body: Box<dyn Read + Send>,
}

/// Parsed `scheme://host[:port]/path?query`.
#[derive(Debug, PartialEq, Eq)]
pub struct Url {
    pub https: bool,
    pub host: String,
    pub port: u16,
    pub path: String,
}

pub fn parse_url(url: &str) -> Result<Url> {
    let (https, rest) = if let Some(r) = url.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (false, r)
    } else {
        bail!("unsupported URL scheme in {url}");
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    if authority.is_empty() || authority.contains('@') {
        bail!("invalid host in {url}");
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if !h.contains(']') || h.ends_with(']') => {
            (h.to_string(), p.parse::<u16>().map_err(|_| anyhow::anyhow!("invalid port in {url}"))?)
        }
        _ => (authority.to_string(), if https { 443 } else { 80 }),
    };
    Ok(Url { https, host, port, path: path.to_string() })
}

#[cfg(windows)]
mod imp {
    use std::io::{self, Read};
    use std::ptr::null;

    use anyhow::{bail, Result};
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::Networking::WinHttp::*;

    use super::{parse_url, HttpResponse};

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    struct Handle(*mut core::ffi::c_void);
    // SAFETY: WinHTTP handles opened in synchronous mode may be used from
    // any thread; ownership moves with the reader.
    unsafe impl Send for Handle {}
    impl Drop for Handle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { WinHttpCloseHandle(self.0) };
            }
        }
    }

    struct Body {
        request: Handle,
        _connect: Handle,
        _session: Handle,
    }

    impl Read for Body {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let mut read = 0u32;
            let want = buf.len().min(u32::MAX as usize) as u32;
            let ok = unsafe { WinHttpReadData(self.request.0, buf.as_mut_ptr().cast(), want, &mut read) };
            if ok == 0 {
                return Err(io::Error::other(format!("network read failed (WinHTTP error {})", unsafe { GetLastError() })));
            }
            Ok(read as usize)
        }
    }

    fn last(what: &str) -> anyhow::Error {
        let code = unsafe { GetLastError() };
        let hint = match code {
            12002 => " (timed out)",
            12007 => " (host name not resolved; are you offline?)",
            12029 | 12030 => " (connection failed or was reset)",
            12175 | 12045 | 12057 => " (TLS certificate problem)",
            _ => "",
        };
        anyhow::anyhow!("{what} failed: WinHTTP error {code}{hint}")
    }

    pub fn get(url: &str, offset: u64) -> Result<HttpResponse> {
        let u = parse_url(url)?;
        unsafe {
            let agent = wide(concat!("SwiftVoice/", env!("CARGO_PKG_VERSION")));
            let mut session = WinHttpOpen(agent.as_ptr(), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, null(), null(), 0);
            if session.is_null() {
                // Windows versions before 8.1 lack automatic proxy mode.
                session = WinHttpOpen(agent.as_ptr(), WINHTTP_ACCESS_TYPE_DEFAULT_PROXY, null(), null(), 0);
            }
            if session.is_null() {
                return Err(last("opening WinHTTP"));
            }
            let session = Handle(session);
            WinHttpSetTimeouts(session.0, 30_000, 30_000, 30_000, 120_000);
            let host = wide(&u.host);
            let connect = Handle(WinHttpConnect(session.0, host.as_ptr(), u.port, 0));
            if connect.0.is_null() {
                return Err(last(&format!("connecting to {}", u.host)));
            }
            let verb = wide("GET");
            let path = wide(&u.path);
            let flags = if u.https { WINHTTP_FLAG_SECURE } else { 0 };
            let request = Handle(WinHttpOpenRequest(connect.0, verb.as_ptr(), path.as_ptr(), null(), null(), null(), flags));
            if request.0.is_null() {
                return Err(last("creating the request"));
            }
            let headers = if offset > 0 { wide(&format!("Range: bytes={offset}-\r\n")) } else { wide("") };
            let headers_len = (headers.len() - 1) as u32;
            let header_ptr = if offset > 0 { headers.as_ptr() } else { null() };
            if WinHttpSendRequest(request.0, header_ptr, headers_len, null(), 0, 0, 0) == 0 {
                return Err(last(&format!("sending the request to {}", u.host)));
            }
            if WinHttpReceiveResponse(request.0, std::ptr::null_mut()) == 0 {
                return Err(last(&format!("receiving the response from {}", u.host)));
            }
            let mut status: u32 = 0;
            let mut size = 4u32;
            if WinHttpQueryHeaders(
                request.0,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                null(),
                (&mut status as *mut u32).cast(),
                &mut size,
                std::ptr::null_mut(),
            ) == 0
            {
                bail!("no HTTP status from {}", u.host);
            }
            let mut length: u64 = 0;
            let mut size = 8u32;
            let content_length = if WinHttpQueryHeaders(
                request.0,
                WINHTTP_QUERY_CONTENT_LENGTH | WINHTTP_QUERY_FLAG_NUMBER64,
                null(),
                (&mut length as *mut u64).cast(),
                &mut size,
                std::ptr::null_mut(),
            ) != 0
            {
                Some(length)
            } else {
                None
            };
            Ok(HttpResponse {
                status: status as u16,
                content_length,
                body: Box::new(Body { request, _connect: connect, _session: session }),
            })
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use std::io::{BufRead, BufReader, Read};
    use std::process::{Command, Stdio};

    use anyhow::{bail, Context, Result};

    use super::HttpResponse;

    struct CurlBody {
        reader: BufReader<std::process::ChildStdout>,
        child: std::process::Child,
    }

    impl Read for CurlBody {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = self.reader.read(buf)?;
            if n == 0 {
                let status = self.child.wait()?;
                if !status.success() {
                    return Err(std::io::Error::other(format!("curl exited with {status}")));
                }
            }
            Ok(n)
        }
    }

    pub fn get(url: &str, offset: u64) -> Result<HttpResponse> {
        let mut cmd = Command::new("curl");
        cmd.args(["-s", "-L", "--max-redirs", "5", "--connect-timeout", "30", "-D", "-", "-o", "-"]);
        if offset > 0 {
            cmd.args(["-r", &format!("{offset}-")]);
        }
        cmd.arg(url).stdout(Stdio::piped()).stderr(Stdio::inherit());
        let mut child = cmd.spawn().context("cannot run curl (needed for downloads on this platform)")?;
        let stdout = child.stdout.take().context("curl stdout")?;
        let mut reader = BufReader::new(stdout);
        // `-D -` writes one header block per response (redirects first).
        loop {
            let mut status_line = String::new();
            if reader.read_line(&mut status_line)? == 0 {
                bail!("no response from {url}");
            }
            let status: u16 = status_line
                .split_whitespace()
                .nth(1)
                .and_then(|s| s.parse().ok())
                .with_context(|| format!("bad status line {status_line:?}"))?;
            let mut content_length = None;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line)? == 0 {
                    bail!("truncated headers from {url}");
                }
                let line = line.trim_end();
                if line.is_empty() {
                    break;
                }
                if let Some((k, v)) = line.split_once(':') {
                    if k.eq_ignore_ascii_case("content-length") {
                        content_length = v.trim().parse().ok();
                    }
                }
            }
            if (300..400).contains(&status) || status == 100 {
                continue;
            }
            return Ok(HttpResponse { status, content_length, body: Box::new(CurlBody { reader, child }) });
        }
    }
}

pub use imp::get;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_urls() {
        let u = parse_url("https://github.com/a/b?x=1").unwrap();
        assert_eq!(u, Url { https: true, host: "github.com".into(), port: 443, path: "/a/b?x=1".into() });
        let u = parse_url("http://127.0.0.1:11434").unwrap();
        assert_eq!(u, Url { https: false, host: "127.0.0.1".into(), port: 11434, path: "/".into() });
        assert!(parse_url("ftp://x").is_err());
        assert!(parse_url("https://user@host/").is_err());
        assert!(parse_url("https://host:notaport/").is_err());
    }
}
