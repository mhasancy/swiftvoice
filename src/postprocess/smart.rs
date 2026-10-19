//! Smart mode: optional rewrite by a LOCAL language model server.
//!
//! Supported servers: Ollama (`/api/chat`) and OpenAI-compatible local
//! servers such as LM Studio (`/v1/chat/completions`). Only plain-HTTP
//! loopback endpoints are accepted (127.0.0.0/8, ::1, "localhost", which is
//! mapped to 127.0.0.1 without consulting DNS or the hosts file), so the
//! text cannot be sent off the machine by a misconfiguration. Audio is never
//! sent; only the cleaned transcript. If the server is slow, down, or
//! returns something implausible, the caller keeps the Clean result.

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

use crate::config::{SmartConfig, SmartProvider};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopbackUrl {
    pub addr: SocketAddr,
    pub host_header: String,
    /// Base path without a trailing slash ("" for the root).
    pub base_path: String,
}

impl LoopbackUrl {
    pub fn parse(url: &str) -> Result<LoopbackUrl, String> {
        let rest = if let Some(r) = url.strip_prefix("http://") {
            r
        } else if url.starts_with("https://") {
            return Err(format!("\"{url}\": use http:// for a local server (loopback traffic never leaves this computer)"));
        } else {
            return Err(format!("\"{url}\" must start with http:// and point to a loopback address"));
        };
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], rest[i..].trim_end_matches('/')),
            None => (rest, ""),
        };
        if authority.contains('@') {
            return Err(format!("\"{url}\": credentials in the URL are not supported"));
        }
        let (host, port) = if let Some(stripped) = authority.strip_prefix('[') {
            let end = stripped.find(']').ok_or_else(|| format!("\"{url}\": bad IPv6 address"))?;
            let host = &stripped[..end];
            let port = stripped[end + 1..].strip_prefix(':').unwrap_or("80");
            (host.to_string(), port)
        } else {
            match authority.rsplit_once(':') {
                Some((h, p)) => (h.to_string(), p),
                None => (authority.to_string(), "80"),
            }
        };
        let port: u16 = port.parse().map_err(|_| format!("\"{url}\": invalid port"))?;
        let ip: IpAddr = if host.eq_ignore_ascii_case("localhost") {
            IpAddr::V4(Ipv4Addr::LOCALHOST)
        } else {
            host.parse::<IpAddr>().map_err(|_| {
                format!("\"{url}\": host must be localhost, 127.x.x.x or ::1 (a loopback address); remote servers are not allowed")
            })?
        };
        let loopback = match ip {
            IpAddr::V4(v4) => v4.is_loopback(),
            IpAddr::V6(v6) => v6 == Ipv6Addr::LOCALHOST,
        };
        if !loopback {
            return Err(format!("\"{url}\": {ip} is not a loopback address; Smart mode only talks to servers on this computer"));
        }
        Ok(LoopbackUrl { addr: SocketAddr::new(ip, port), host_header: authority.to_string(), base_path: path.to_string() })
    }
}

/// POST a JSON body and return (status, body). Fails after `timeout`.
pub fn post_json(url: &LoopbackUrl, path: &str, body: &str, timeout: Duration) -> Result<(u16, String)> {
    let deadline = Instant::now() + timeout;
    let mut stream = TcpStream::connect_timeout(&url.addr, timeout)
        .with_context(|| format!("cannot connect to the local model server at {} (is it running?)", url.addr))?;
    stream.set_nodelay(true).ok();
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nAccept: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        url.host_header,
        body.len()
    );
    stream.set_write_timeout(Some(remaining(deadline)?))?;
    stream.write_all(request.as_bytes())?;
    stream.write_all(body.as_bytes())?;
    let mut raw = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        stream.set_read_timeout(Some(remaining(deadline)?))?;
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                raw.extend_from_slice(&buf[..n]);
                if raw.len() > 4 * 1024 * 1024 {
                    bail!("local model server response is too large");
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => {
                bail!("local model server did not answer within {} ms", timeout.as_millis())
            }
            Err(e) => return Err(e.into()),
        }
    }
    parse_response(&raw)
}

fn remaining(deadline: Instant) -> Result<Duration> {
    let now = Instant::now();
    if now >= deadline {
        bail!("local model server timed out");
    }
    Ok(deadline - now)
}

fn parse_response(raw: &[u8]) -> Result<(u16, String)> {
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").context("malformed HTTP response")?;
    let head = std::str::from_utf8(&raw[..split]).context("malformed HTTP headers")?;
    let mut lines = head.split("\r\n");
    let status: u16 =
        lines.next().and_then(|l| l.split_whitespace().nth(1)).and_then(|s| s.parse().ok()).context("malformed HTTP status line")?;
    let mut chunked = false;
    let mut length: Option<usize> = None;
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            let (k, v) = (k.trim(), v.trim());
            if k.eq_ignore_ascii_case("transfer-encoding") && v.to_ascii_lowercase().contains("chunked") {
                chunked = true;
            } else if k.eq_ignore_ascii_case("content-length") {
                length = v.parse().ok();
            }
        }
    }
    let mut body = &raw[split + 4..];
    let decoded;
    if chunked {
        decoded = dechunk(body)?;
        body = &decoded;
    } else if let Some(n) = length {
        body = &body[..n.min(body.len())];
    }
    Ok((status, String::from_utf8_lossy(body).into_owned()))
}

fn dechunk(mut data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let line_end = data.windows(2).position(|w| w == b"\r\n").context("truncated chunked body")?;
        let size_str = std::str::from_utf8(&data[..line_end])?.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_str, 16).context("bad chunk size")?;
        data = &data[line_end + 2..];
        if size == 0 {
            return Ok(out);
        }
        if data.len() < size + 2 {
            bail!("truncated chunk");
        }
        out.extend_from_slice(&data[..size]);
        data = &data[size + 2..];
    }
}

pub struct SmartRewriter {
    cfg: SmartConfig,
    url: LoopbackUrl,
}

impl SmartRewriter {
    pub fn new(cfg: &SmartConfig) -> Result<SmartRewriter> {
        let url = LoopbackUrl::parse(&cfg.endpoint).map_err(|e| anyhow!(e))?;
        Ok(SmartRewriter { cfg: cfg.clone(), url })
    }

    fn path(&self) -> String {
        let base = &self.url.base_path;
        match self.cfg.provider {
            SmartProvider::Ollama => format!("{base}/api/chat"),
            SmartProvider::Openai if base.ends_with("/v1") => format!("{base}/chat/completions"),
            SmartProvider::Openai => format!("{base}/v1/chat/completions"),
        }
    }

    pub fn request_body(&self, text: &str, language: Option<&str>) -> String {
        let mut system = self.cfg.prompt.clone();
        if let Some(lang) = language.filter(|l| *l != "auto") {
            system.push_str(&format!(" The text is in language code \"{lang}\"; answer in the same language."));
        }
        let messages = json!([
            {"role": "system", "content": system},
            {"role": "user", "content": text},
        ]);
        let body = match self.cfg.provider {
            SmartProvider::Ollama => json!({
                "model": self.cfg.model,
                "messages": messages,
                "stream": false,
                "think": false,
                "keep_alive": "15m",
                "options": {"temperature": self.cfg.temperature},
            }),
            SmartProvider::Openai => json!({
                "model": self.cfg.model,
                "messages": messages,
                "stream": false,
                "temperature": self.cfg.temperature,
            }),
        };
        body.to_string()
    }

    /// Rewrite `text`; errors mean "keep the Clean result".
    pub fn rewrite(&self, text: &str, language: Option<&str>) -> Result<String> {
        let timeout = Duration::from_millis(self.cfg.timeout_ms as u64);
        let (status, body) = post_json(&self.url, &self.path(), &self.request_body(text, language), timeout)?;
        if status != 200 {
            log::trace!("local model server error response: {body}");
            bail!("local model server answered HTTP {status}");
        }
        let v: Value = serde_json::from_str(&body).context("local model server returned invalid JSON")?;
        let content = match self.cfg.provider {
            SmartProvider::Ollama => v.pointer("/message/content"),
            SmartProvider::Openai => v.pointer("/choices/0/message/content"),
        }
        .and_then(Value::as_str)
        .context("local model server response has no message content")?;
        let cleaned = strip_model_artifacts(content);
        check_plausible(text, &cleaned)?;
        let leading = text.len() - text.trim_start_matches('\n').len();
        let trailing = text.len() - text.trim_end_matches('\n').len();
        Ok(format!("{}{}{}", &text[..leading], cleaned, &text[text.len() - trailing..]))
    }
}

/// Remove reasoning blocks and wrapping quotes some models add.
pub fn strip_model_artifacts(s: &str) -> String {
    let mut out = s.to_string();
    while let (Some(start), Some(end)) = (out.find("<think>"), out.find("</think>")) {
        if end < start {
            break;
        }
        out.replace_range(start..end + "</think>".len(), "");
    }
    let mut t = out.trim();
    for (open, close) in [("\"", "\""), ("“", "”"), ("'", "'")] {
        if t.len() > open.len() + close.len() && t.starts_with(open) && t.ends_with(close) {
            let inner = &t[open.len()..t.len() - close.len()];
            if !inner.contains(open) {
                t = inner.trim();
            }
        }
    }
    t.to_string()
}

/// Reject rewrites that are empty or wildly different in length — those
/// are almost always the model chatting instead of editing.
pub fn check_plausible(input: &str, output: &str) -> Result<()> {
    let n_in = input.chars().count() as f64;
    let n_out = output.chars().count() as f64;
    if output.trim().is_empty() {
        bail!("the local model returned an empty rewrite");
    }
    if n_out > n_in * 2.0 + 40.0 || (n_in > 20.0 && n_out < n_in * 0.4) {
        bail!("the local model changed the text length implausibly ({} -> {} characters); keeping the Clean text", n_in, n_out);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn accepts_only_loopback_http() {
        assert!(LoopbackUrl::parse("http://127.0.0.1:11434").is_ok());
        assert!(LoopbackUrl::parse("http://localhost:1234/v1").is_ok());
        assert!(LoopbackUrl::parse("http://[::1]:8080").is_ok());
        assert!(LoopbackUrl::parse("http://127.5.6.7:80/").is_ok());
        for bad in [
            "http://192.168.1.10:11434",
            "http://example.com:11434",
            "https://127.0.0.1:11434",
            "ftp://127.0.0.1",
            "http://user@127.0.0.1:1",
            "http://0.0.0.0:11434",
            "http://localhost.evil.com:1",
        ] {
            let err = LoopbackUrl::parse(bad).unwrap_err();
            assert!(!err.is_empty(), "{bad}");
        }
        let u = LoopbackUrl::parse("http://localhost:1234/v1/").unwrap();
        assert_eq!(u.addr, "127.0.0.1:1234".parse().unwrap());
        assert_eq!(u.base_path, "/v1");
    }

    fn json_response(body: &str) -> Vec<u8> {
        format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).into_bytes()
    }

    fn chunked_response(parts: &[&str]) -> Vec<u8> {
        let mut out = String::from("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n");
        for p in parts {
            out.push_str(&format!("{:x}\r\n{p}\r\n", p.len()));
        }
        out.push_str("0\r\n\r\n");
        out.into_bytes()
    }

    fn serve_once(response: Vec<u8>, delay_ms: u64) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut req = Vec::new();
            let mut buf = [0u8; 4096];
            // Read headers + body (Content-Length based).
            loop {
                let n = sock.read(&mut buf).unwrap();
                req.extend_from_slice(&buf[..n]);
                if let Some(p) = req.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&req[..p]).to_string();
                    let len: usize =
                        head.lines().find_map(|l| l.strip_prefix("Content-Length: ").map(|v| v.trim().parse().unwrap())).unwrap_or(0);
                    if req.len() >= p + 4 + len {
                        break;
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(delay_ms));
            let _ = sock.write_all(&response);
            String::from_utf8_lossy(&req).to_string()
        });
        (format!("http://127.0.0.1:{}", addr.port()), handle)
    }

    fn cfg(endpoint: &str, provider: SmartProvider, timeout_ms: u32) -> SmartConfig {
        SmartConfig { endpoint: endpoint.into(), provider, timeout_ms, ..SmartConfig::default() }
    }

    #[test]
    fn ollama_round_trip() {
        let (url, server) =
            serve_once(json_response(r#"{"message":{"role":"assistant","content":"We should ship it today."},"done":true}"#), 0);
        let r = SmartRewriter::new(&cfg(&url, SmartProvider::Ollama, 3000)).unwrap();
        let out = r.rewrite("so we should uh ship it today", Some("en")).unwrap();
        assert_eq!(out, "We should ship it today.");
        let req = server.join().unwrap();
        assert!(req.starts_with("POST /api/chat HTTP/1.1"));
        assert!(req.contains("\"stream\":false"));
    }

    #[test]
    fn openai_compatible_chunked_round_trip() {
        let (url, server) = serve_once(chunked_response(&[r#"{"choices":[{"message":{"#, r#""content":"<think>x</think>Ok."}}]}"#]), 0);
        let r = SmartRewriter::new(&cfg(&format!("{url}/v1"), SmartProvider::Openai, 3000)).unwrap();
        assert_eq!(r.rewrite("ok", None).unwrap(), "Ok.");
        assert!(server.join().unwrap().starts_with("POST /v1/chat/completions"));
    }

    #[test]
    fn server_errors_do_not_expose_response_content() {
        let body = "synthetic private dictation";
        let response = format!("HTTP/1.1 400 Bad Request\r\nContent-Length: {}\r\n\r\n{body}", body.len());
        let (url, server) = serve_once(response.into_bytes(), 0);
        let r = SmartRewriter::new(&cfg(&url, SmartProvider::Ollama, 3000)).unwrap();
        assert_eq!(r.rewrite(body, None).unwrap_err().to_string(), "local model server answered HTTP 400");
        server.join().unwrap();
    }

    #[test]
    fn smart_rewrite_keeps_command_newlines() {
        let (url, server) = serve_once(json_response(r#"{"message":{"content":"Hello there."}}"#), 0);
        let r = SmartRewriter::new(&cfg(&url, SmartProvider::Ollama, 3000)).unwrap();
        assert_eq!(r.rewrite("\nhello there\n\n", None).unwrap(), "\nHello there.\n\n");
        server.join().unwrap();
    }

    #[test]
    fn slow_server_times_out() {
        let (url, server) = serve_once(json_response("{}"), 1500);
        let r = SmartRewriter::new(&cfg(&url, SmartProvider::Ollama, 300)).unwrap();
        let started = Instant::now();
        let err = r.rewrite("hello there", None).unwrap_err().to_string();
        assert!(started.elapsed() < Duration::from_millis(1200), "timeout not enforced");
        assert!(err.contains("ms") || err.contains("timed out"), "{err}");
        let _ = server.join();
    }

    #[test]
    fn server_down_is_a_clear_error() {
        // Bind and drop to get a port that is (almost certainly) closed.
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let r = SmartRewriter::new(&cfg(&format!("http://127.0.0.1:{port}"), SmartProvider::Ollama, 500)).unwrap();
        let err = format!("{:#}", r.rewrite("hello", None).unwrap_err());
        assert!(err.contains("is it running"), "{err}");
    }

    #[test]
    fn implausible_rewrites_are_rejected() {
        assert!(check_plausible("ship it", "Sure! Here is a much longer answer about shipping that goes on and on and on and on").is_err());
        assert!(check_plausible("a reasonably long sentence about the plan for today", "Plan.").is_err());
        assert!(check_plausible("we ship today", "We ship today.").is_ok());
        assert_eq!(strip_model_artifacts("\"Hello there.\""), "Hello there.");
        assert_eq!(strip_model_artifacts("<think>\nhmm\n</think>\nFine."), "Fine.");
    }
}
