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

    pub fn get_json(&self, url: &str) -> Result<Value> {
        let (status, body) = self.perform("GET", url, None)?;
        decode_status_body(status, &body)
    }

    pub fn post_form(&self, url: &str, form: &[(&str, String)]) -> Result<Value> {
        let encoded: String = form
            .iter()
            .map(|(k, v)| {
                format!(
                    "{}={}",
                    urlencoding_encode(k),
                    urlencoding_encode(v)
                )
            })
            .collect::<Vec<_>>()
            .join("&");
        let (status, body) = self.perform("POST", url, Some(encoded))?;
        decode_status_body(status, &body)
    }

    pub fn get_bytes(&self, url: &str) -> Result<Vec<u8>> {
        let (status, body) = self.perform("GET", url, None)?;
        if status == 429 || status == 401 || status == 403 {
            return Err(ImagoError::RateLimited(format!("download HTTP {status}")));
        }
        if !(200..300).contains(&status) {
            return Err(ImagoError::Network(format!("download HTTP {status}")));
        }
        if body.is_empty() {
            return Err(ImagoError::Network("empty download body".into()));
        }
        Ok(body)
    }

    fn perform(&self, method: &str, url: &str, body: Option<String>) -> Result<(u32, Vec<u8>)> {
        let mut easy = Easy::new();
        easy.url(url)
            .map_err(|e| ImagoError::Network(e.to_string()))?;
        easy.useragent(&self.user_agent)
            .map_err(|e| ImagoError::Network(e.to_string()))?;
        easy.timeout(Duration::from_secs(60))
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
        headers
            .append(&format!(
                "Cookie: sessionid={}; csrftoken={}",
                self.session_id, self.csrf_token
            ))
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
        {
            let mut transfer = easy.transfer();
            transfer
                .write_function(|data| {
                    buf.extend_from_slice(data);
                    Ok(data.len())
                })
                .map_err(|e| ImagoError::Network(e.to_string()))?;
            transfer
                .perform()
                .map_err(|e| ImagoError::Network(e.to_string()))?;
        }

        let status = easy
            .response_code()
            .map_err(|e| ImagoError::Network(e.to_string()))?;
        Ok((status, buf))
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
        let msg = v
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("fail");
        let lower = msg.to_lowercase();
        if lower.contains("please wait")
            || lower.contains("rate")
            || lower.contains("try again")
        {
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
