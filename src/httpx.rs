//! HTTP via libcurl — Instagram's edge blocks common Rust TLS fingerprints
//! (reqwest/rustls/native-tls get bare 429) while curl's stack is accepted.

use std::time::Duration;

use curl::easy::{Easy, List};
use serde_json::Value;

use crate::auth::Credentials;
use crate::error::{ImagoError, Result};

#[derive(Clone)]
pub struct CurlHttp {
    user_agent: String,
    session_id: String,
    csrf_token: String,
}

impl CurlHttp {
    pub fn new(creds: &Credentials, user_agent: &str) -> Self {
        let ua = creds
            .user_agent
            .clone()
            .unwrap_or_else(|| user_agent.to_string());
        Self {
            user_agent: ua,
            session_id: creds.session_id.clone(),
            csrf_token: creds.csrf_token.clone(),
        }
    }

    /// The logged-in account id (`ds_user_id`), read from the sessionid prefix.
    pub fn account_id(&self) -> Option<String> {
        ds_user_id(&self.session_id)
    }

    pub fn get_json(&self, url: &str) -> Result<Value> {
        let (status, _total, body) = self.perform("GET", url, None, None)?;
        decode_status_body(status, &body)
    }

    pub fn post_form(&self, url: &str, form: &[(&str, String)]) -> Result<Value> {
        let encoded: String = form
            .iter()
            .map(|(k, v)| format!("{}={}", urlencoding_encode(k), urlencoding_encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        let (status, _total, body) = self.perform("POST", url, Some(encoded), None)?;
        decode_status_body(status, &body)
    }

    /// Download an asset, resuming through explicit byte ranges.
    ///
    /// Instagram's video CDN throttles an open-ended GET to the first ~512KB then
    /// stalls the connection, but serves bounded `Range` requests instantly. Pulling
    /// the file in fixed chunks sidesteps the truncation; small assets (images) still
    /// finish in a single request.
    pub fn get_bytes(&self, url: &str) -> Result<Vec<u8>> {
        const CHUNK: u64 = 1 << 20;
        const MAX_BYTES: u64 = 512 << 20;
        let mut buf: Vec<u8> = Vec::new();
        let mut total: Option<u64> = None;
        loop {
            let start = buf.len() as u64;
            if total.map(|t| start >= t).unwrap_or(false) {
                break;
            }
            let (status, ctotal, chunk) =
                self.perform("GET", url, None, Some((start, start + CHUNK - 1)))?;
            if status == 416 {
                break;
            }
            if status == 429 || status == 401 || status == 403 {
                return Err(ImagoError::RateLimited(format!("download HTTP {status}")));
            }
            if !(200..300).contains(&status) {
                return Err(ImagoError::Network(format!("download HTTP {status}")));
            }
            if status == 200 && start > 0 {
                buf = chunk;
                break;
            }
            if let Some(t) = ctotal {
                total = Some(t);
            }
            let got = chunk.len() as u64;
            buf.extend_from_slice(&chunk);
            if got == 0 || (total.is_none() && got < CHUNK) || buf.len() as u64 > MAX_BYTES {
                break;
            }
        }
        if buf.is_empty() {
            return Err(ImagoError::Network("empty download body".into()));
        }
        Ok(buf)
    }

    fn perform(
        &self,
        method: &str,
        url: &str,
        body: Option<String>,
        range: Option<(u64, u64)>,
    ) -> Result<(u32, Option<u64>, Vec<u8>)> {
        let mut easy = Easy::new();
        easy.url(url)
            .map_err(|e| ImagoError::Network(e.to_string()))?;
        if let Some((start, end)) = range {
            easy.range(&format!("{start}-{end}"))
                .map_err(|e| ImagoError::Network(e.to_string()))?;
        }
        easy.useragent(&self.user_agent)
            .map_err(|e| ImagoError::Network(e.to_string()))?;
        easy.timeout(Duration::from_secs(300))
            .map_err(|e| ImagoError::Network(e.to_string()))?;
        easy.connect_timeout(Duration::from_secs(20))
            .map_err(|e| ImagoError::Network(e.to_string()))?;
        easy.low_speed_limit(1024)
            .map_err(|e| ImagoError::Network(e.to_string()))?;
        easy.low_speed_time(Duration::from_secs(20))
            .map_err(|e| ImagoError::Network(e.to_string()))?;
        easy.follow_location(false)
            .map_err(|e| ImagoError::Network(e.to_string()))?;
        easy.accept_encoding("") // let curl handle gzip
            .map_err(|e| ImagoError::Network(e.to_string()))?;

        let mut headers = List::new();
        headers
            .append("Accept: */*")
            .map_err(|e| ImagoError::Network(e.to_string()))?;
        headers
            .append("Accept-Language: en-US,en;q=0.9")
            .map_err(|e| ImagoError::Network(e.to_string()))?;
        headers
            .append("X-IG-App-ID: 936619743392459")
            .map_err(|e| ImagoError::Network(e.to_string()))?;
        headers
            .append("X-Requested-With: XMLHttpRequest")
            .map_err(|e| ImagoError::Network(e.to_string()))?;
        headers
            .append("Referer: https://www.instagram.com/")
            .map_err(|e| ImagoError::Network(e.to_string()))?;
        headers
            .append(&format!("X-CSRFToken: {}", self.csrf_token))
            .map_err(|e| ImagoError::Network(e.to_string()))?;
        let cookie = match ds_user_id(&self.session_id) {
            Some(uid) => format!(
                "sessionid={}; csrftoken={}; ds_user_id={uid}",
                self.session_id, self.csrf_token
            ),
            None => format!(
                "sessionid={}; csrftoken={}",
                self.session_id, self.csrf_token
            ),
        };
        headers
            .append(&format!("Cookie: {cookie}"))
            .map_err(|e| ImagoError::Network(e.to_string()))?;

        if body.is_some() {
            headers
                .append("Content-Type: application/x-www-form-urlencoded")
                .map_err(|e| ImagoError::Network(e.to_string()))?;
        }

        easy.http_headers(headers)
            .map_err(|e| ImagoError::Network(e.to_string()))?;

        if method == "POST" {
            easy.post(true)
                .map_err(|e| ImagoError::Network(e.to_string()))?;
            if let Some(ref b) = body {
                easy.post_fields_copy(b.as_bytes())
                    .map_err(|e| ImagoError::Network(e.to_string()))?;
            }
        }

        let mut buf = Vec::new();
        let mut range_total: Option<u64> = None;
        let mut content_length: Option<u64> = None;
        {
            let mut transfer = easy.transfer();
            transfer
                .write_function(|data| {
                    buf.extend_from_slice(data);
                    Ok(data.len())
                })
                .map_err(|e| ImagoError::Network(e.to_string()))?;
            transfer
                .header_function(|line| {
                    match parse_size_header(line) {
                        Some(SizeHeader::ContentRangeTotal(t)) => range_total = Some(t),
                        Some(SizeHeader::ContentLength(l)) => content_length = Some(l),
                        None => {}
                    }
                    true
                })
                .map_err(|e| ImagoError::Network(e.to_string()))?;
            transfer
                .perform()
                .map_err(|e| ImagoError::Network(e.to_string()))?;
        }

        let status = easy
            .response_code()
            .map_err(|e| ImagoError::Network(e.to_string()))?;
        Ok((status, range_total.or(content_length), buf))
    }
}

enum SizeHeader {
    ContentRangeTotal(u64),
    ContentLength(u64),
}

/// Read the full asset size from `Content-Range: bytes S-E/TOTAL` (authoritative for
/// partial responses) or `Content-Length` (a non-range GET returning the whole file).
fn parse_size_header(line: &[u8]) -> Option<SizeHeader> {
    let line = std::str::from_utf8(line).ok()?;
    let (name, value) = line.split_once(':')?;
    let value = value.trim();
    match name.trim().to_ascii_lowercase().as_str() {
        "content-range" => value
            .rsplit('/')
            .next()
            .and_then(|t| t.trim().parse().ok())
            .map(SizeHeader::ContentRangeTotal),
        "content-length" => value.parse().ok().map(SizeHeader::ContentLength),
        _ => None,
    }
}

fn decode_status_body(status: u32, body: &[u8]) -> Result<Value> {
    let text = String::from_utf8_lossy(body);
    if status == 301 || status == 302 {
        if text.contains("login") {
            return Err(ImagoError::SessionDead);
        }
        return Err(ImagoError::RateLimited(format!("redirect HTTP {status}")));
    }
    if status == 429 {
        return Err(ImagoError::RateLimited("HTTP 429".into()));
    }
    if status == 401 || status == 403 {
        let lower = text.to_lowercase();
        if lower.contains("please wait")
            || lower.contains("rate")
            || lower.contains("try again")
            || lower.is_empty()
        {
            return Err(ImagoError::RateLimited(format!("HTTP {status}")));
        }
        if lower.contains("login_required")
            || lower.contains("checkpoint")
            || lower.contains("challenge")
        {
            return Err(ImagoError::SessionDead);
        }
        return Err(ImagoError::RateLimited(format!(
            "HTTP {status} (treating as temporary block)"
        )));
    }
    if !(200..300).contains(&status) {
        if status >= 500 {
            return Err(ImagoError::Network(format!(
                "HTTP {status}: {}",
                text.chars().take(200).collect::<String>()
            )));
        }
        return Err(ImagoError::Network(format!(
            "HTTP {status}: {}",
            text.chars().take(200).collect::<String>()
        )));
    }

    let v: Value = serde_json::from_slice(body).map_err(|e| {
        ImagoError::Parse(format!(
            "json: {e}; body={}",
            text.chars().take(200).collect::<String>()
        ))
    })?;

    if v.get("status").and_then(|s| s.as_str()) == Some("fail") {
        let msg = v.get("message").and_then(|m| m.as_str()).unwrap_or("fail");
        let lower = msg.to_lowercase();
        if lower.contains("please wait") || lower.contains("rate") || lower.contains("try again") {
            return Err(ImagoError::RateLimited(msg.into()));
        }
        if v.get("require_login").and_then(|b| b.as_bool()) == Some(true)
            || lower.contains("login_required")
            || lower.contains("checkpoint")
            || lower.contains("challenge")
        {
            return Err(ImagoError::SessionDead);
        }
        return Err(ImagoError::RateLimited(msg.into()));
    }
    Ok(v)
}

/// Instagram's `sessionid` is `<user_id>%3A<token>%3A<...>` (URL-encoded `:`),
/// so the leading digits are the account's `ds_user_id`. Authenticated endpoints
/// 302-redirect when `sessionid` is sent without a matching `ds_user_id` cookie.
fn ds_user_id(session_id: &str) -> Option<String> {
    let uid: String = session_id
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if uid.is_empty() {
        None
    } else {
        Some(uid)
    }
}

fn urlencoding_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_size_headers() {
        assert!(matches!(
            parse_size_header(b"Content-Range: bytes 0-1048575/6352563\r\n"),
            Some(SizeHeader::ContentRangeTotal(6352563))
        ));
        assert!(matches!(
            parse_size_header(b"content-length: 200000\r\n"),
            Some(SizeHeader::ContentLength(200000))
        ));
        assert!(parse_size_header(b"Content-Type: video/mp4\r\n").is_none());
    }

    #[test]
    fn derives_ds_user_id() {
        assert_eq!(
            ds_user_id("192008031%3A4CSxhQTRiiyCEG%3A1%3AABC").as_deref(),
            Some("192008031")
        );
        assert_eq!(
            ds_user_id("192008031:token:1").as_deref(),
            Some("192008031")
        );
        assert_eq!(ds_user_id(""), None);
        assert_eq!(ds_user_id("%3Anope"), None);
    }
}
