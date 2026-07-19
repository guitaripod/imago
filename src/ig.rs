use serde_json::{json, Value};
use tracing::debug;

use crate::auth::Credentials;
use crate::error::{ImagoError, Result};
use crate::httpx::CurlHttp;
use crate::media::{self, expand_connection, is_feed_cursor, Page};

const BASE: &str = "https://www.instagram.com";
const DOC_ID_LOGGED_IN: &str = "7898261790222653";
const DOC_ID_LOGGED_OUT: &str = "7950326061742207";

pub struct IgClient {
    http: CurlHttp,
}

impl IgClient {
    pub fn new(creds: &Credentials, user_agent: &str) -> Result<Self> {
        Ok(Self {
            http: CurlHttp::new(creds, user_agent),
        })
    }

    fn http(&self) -> CurlHttp {
        self.http.clone()
    }

    pub async fn fetch_profile_page(&self, username: &str) -> Result<Page> {
        let url = format!("{BASE}/api/v1/users/web_profile_info/?username={username}");
        let http = self.http();
        let v = tokio::task::spawn_blocking(move || http.get_json(&url))
            .await
            .map_err(|e| ImagoError::Other(e.to_string()))??;

        if v.get("requires_to_login").and_then(|b| b.as_bool()) == Some(true) {
            return Err(ImagoError::SessionDead);
        }
        let user = v
            .pointer("/data/user")
            .ok_or_else(|| ImagoError::NotFound(username.into()))?;
        let user_id = media::stringify_id(user.get("id").unwrap_or(&Value::Null))
            .ok_or_else(|| ImagoError::Parse("profile missing id".into()))?;
        let uname = user
            .get("username")
            .and_then(|u| u.as_str())
            .unwrap_or(username)
            .to_string();
        let media = user
            .get("edge_owner_to_timeline_media")
            .cloned()
            .unwrap_or(Value::Null);
        let count = media.get("count").and_then(|c| c.as_u64());
        let (assets, post_keys, has_next, end_cursor) = expand_connection(&media);
        Ok(Page {
            user_id,
            username: uname,
            media_count: count,
            assets,
            post_keys,
            has_next,
            end_cursor,
        })
    }

    pub async fn fetch_media_page(
        &self,
        user_id: &str,
        username: &str,
        after: Option<&str>,
    ) -> Result<Page> {
        let mut last_err: Option<ImagoError> = None;

        if after.map(is_feed_cursor).unwrap_or(true) {
            match self.fetch_feed(user_id, after).await {
                Ok(p) => return Ok(p),
                Err(e) => {
                    debug!(error = %e, "feed path failed");
                    last_err = Some(e);
                }
            }
        }

        match self.fetch_doc_id(user_id, username, after).await {
            Ok(p) => return Ok(p),
            Err(e) => {
                debug!(error = %e, "doc_id path failed");
                last_err = Some(e);
            }
        }

        Err(last_err.unwrap_or_else(|| ImagoError::Other("media fetch failed".into())))
    }

    async fn fetch_feed(&self, user_id: &str, after: Option<&str>) -> Result<Page> {
        let mut url = format!("{BASE}/api/v1/feed/user/{user_id}/?count=12");
        if let Some(a) = after {
            if !a.is_empty() {
                url.push_str(&format!("&max_id={a}"));
            }
        }
        let http = self.http();
        let v = tokio::task::spawn_blocking(move || http.get_json(&url))
            .await
            .map_err(|e| ImagoError::Other(e.to_string()))??;
        let (assets, post_keys, has_next, end_cursor) = expand_connection(&v);
        Ok(Page {
            user_id: user_id.to_string(),
            username: String::new(),
            media_count: None,
            assets,
            post_keys,
            has_next,
            end_cursor,
        })
    }

    async fn fetch_doc_id(
        &self,
        user_id: &str,
        username: &str,
        after: Option<&str>,
    ) -> Result<Page> {
        let mut variables = json!({
            "data": {
                "count": 12,
                "include_relationship_info": true,
                "latest_besties_reel_media": true,
                "latest_reel_media": true
            },
            "__relay_internal__pv__PolarisFeedShareMenurelayprovider": false
        });
        let doc_id = if !username.is_empty() {
            variables["username"] = json!(username);
            DOC_ID_LOGGED_IN
        } else {
            variables["id"] = json!(user_id);
            DOC_ID_LOGGED_OUT
        };
        if let Some(a) = after {
            variables["after"] = json!(a);
            variables["before"] = Value::Null;
            variables["first"] = json!(12);
            variables["last"] = Value::Null;
        }
        let vars = serde_json::to_string(&variables)?;
        let form = vec![
            ("variables", vars),
            ("doc_id", doc_id.to_string()),
            ("server_timestamps", "true".into()),
        ];
        let url = format!("{BASE}/graphql/query");
        let http = self.http();
        let v = tokio::task::spawn_blocking(move || http.post_form(&url, &form))
            .await
            .map_err(|e| ImagoError::Other(e.to_string()))??;

        let conn = extract_connection(&v).ok_or_else(|| {
            ImagoError::Parse("GraphQL response missing media connection".into())
        })?;
        let (assets, post_keys, has_next, end_cursor) = expand_connection(&conn);
        Ok(Page {
            user_id: user_id.to_string(),
            username: username.to_string(),
            media_count: None,
            assets,
            post_keys,
            has_next,
            end_cursor,
        })
    }

    pub async fn download_bytes(&self, url: &str) -> Result<Vec<u8>> {
        let http = self.http();
        let url = url.to_string();
        tokio::task::spawn_blocking(move || http.get_bytes(&url))
            .await
            .map_err(|e| ImagoError::Other(e.to_string()))?
    }

    pub async fn probe_session(&self) -> Result<String> {
        let page = self.fetch_profile_page("instagram").await?;
        Ok(page.username)
    }
}

fn extract_connection(raw: &Value) -> Option<Value> {
    let data = raw.get("data")?;
    if let Some(c) = data.get("xdt_api__v1__feed__user_timeline_graphql_connection") {
        return Some(c.clone());
    }
    if let Some(user) = data.get("user") {
        if let Some(media) = user.get("edge_owner_to_timeline_media") {
            return Some(media.clone());
        }
        if user.get("edges").is_some() {
            return Some(user.clone());
        }
    }
    None
}

/// Parse profile URL / @user / bare username.
pub fn parse_profile_input(input: &str) -> Result<String> {
    let s = input.trim();
    if s.is_empty() {
        return Err(ImagoError::Usage("empty profile input".into()));
    }

    let s = s.strip_prefix('@').unwrap_or(s);

    if s.contains("instagram.com") {
        let url = if s.starts_with("http") {
            s.to_string()
        } else {
            format!("https://{s}")
        };
        let parsed = url::Url::parse(&url)
            .map_err(|e| ImagoError::Usage(format!("invalid URL: {e}")))?;
        let mut segs: Vec<&str> = parsed
            .path_segments()
            .map(|p| p.filter(|s| !s.is_empty()).collect())
            .unwrap_or_default();
        segs.retain(|x| !x.is_empty());
        if segs.is_empty() {
            return Err(ImagoError::Usage(
                "URL has no username path — expected https://instagram.com/<user>/".into(),
            ));
        }
        let first = segs[0];
        if matches!(first, "p" | "reel" | "reels" | "stories" | "tv") {
            return Err(ImagoError::Usage(
                "post/reel/story URLs are not supported — pass a profile URL".into(),
            ));
        }
        return validate_username(first);
    }

    validate_username(s)
}

fn validate_username(s: &str) -> Result<String> {
    let s = s.trim().trim_end_matches('/');
    if s.is_empty() || s.len() > 30 {
        return Err(ImagoError::Usage(format!("invalid username: {s}")));
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
    {
        return Err(ImagoError::Usage(format!("invalid username: {s}")));
    }
    Ok(s.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_urls() {
        assert_eq!(
            parse_profile_input("https://www.instagram.com/zuck/").unwrap(),
            "zuck"
        );
        assert_eq!(parse_profile_input("@NatGeo").unwrap(), "natgeo");
        assert_eq!(parse_profile_input("instagram.com/foo").unwrap(), "foo");
        assert!(parse_profile_input("https://instagram.com/p/ABC/").is_err());
    }
}
