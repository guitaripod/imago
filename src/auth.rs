use serde::{Deserialize, Serialize};

use crate::appdata;
use crate::error::{ImagoError, Result};

const KEYRING_SERVICE: &str = "imago";
const KEYRING_USER: &str = "default";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credentials {
    pub session_id: String,
    pub csrf_token: String,
    #[serde(default)]
    pub user_agent: Option<String>,
}

impl Credentials {
    pub fn is_valid_shape(&self) -> bool {
        !self.session_id.is_empty()
            && self.session_id.len() > 20
            && !self.csrf_token.is_empty()
            && self.csrf_token.len() >= 8
    }
}

pub fn load(session_flag: Option<&str>, csrf_flag: Option<&str>) -> Result<Credentials> {
    if let (Some(s), Some(c)) = (session_flag, csrf_flag) {
        let creds = Credentials {
            session_id: s.to_string(),
            csrf_token: c.to_string(),
            user_agent: None,
        };
        if creds.is_valid_shape() {
            return Ok(creds);
        }
    }

    if let Ok(s) = std::env::var("IMAGO_SESSION_ID") {
        if let Ok(c) = std::env::var("IMAGO_CSRF_TOKEN") {
            let creds = Credentials {
                session_id: s,
                csrf_token: c,
                user_agent: std::env::var("IMAGO_USER_AGENT").ok(),
            };
            if creds.is_valid_shape() {
                return Ok(creds);
            }
        }
    }

    // Back-compat with igscraper env names during migration
    if let Ok(s) = std::env::var("IGSCRAPER_SESSION_ID") {
        if let Ok(c) = std::env::var("IGSCRAPER_CSRF_TOKEN") {
            let creds = Credentials {
                session_id: s,
                csrf_token: c,
                user_agent: std::env::var("IGSCRAPER_USER_AGENT").ok(),
            };
            if creds.is_valid_shape() {
                return Ok(creds);
            }
        }
    }

    if let Some(creds) = load_file()? {
        return Ok(creds);
    }

    if let Some(creds) = load_keyring()? {
        return Ok(creds);
    }

    Err(ImagoError::Auth(
        "no credentials — set IMAGO_SESSION_ID/IMAGO_CSRF_TOKEN or run: imago auth login".into(),
    ))
}

pub fn store(creds: &Credentials) -> Result<()> {
    if !creds.is_valid_shape() {
        return Err(ImagoError::Usage(
            "session_id and csrf_token look invalid".into(),
        ));
    }
    let path = appdata::credentials_path()?;
    appdata::atomic_write_json(&path, creds)?;
    // Best-effort keyring
    if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER) {
        if let Ok(json) = serde_json::to_string(creds) {
            let _ = entry.set_password(&json);
        }
    }
    Ok(())
}

pub fn clear() -> Result<()> {
    let path = appdata::credentials_path()?;
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER) {
        let _ = entry.delete_credential();
    }
    Ok(())
}

fn load_file() -> Result<Option<Credentials>> {
    let path = appdata::credentials_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)?;
    let creds: Credentials = serde_json::from_str(&raw)?;
    if creds.is_valid_shape() {
        Ok(Some(creds))
    } else {
        Ok(None)
    }
}

fn load_keyring() -> Result<Option<Credentials>> {
    let entry = match keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER) {
        Ok(e) => e,
        Err(_) => return Ok(None),
    };
    let json = match entry.get_password() {
        Ok(p) => p,
        Err(_) => return Ok(None),
    };
    let creds: Credentials = match serde_json::from_str(&json) {
        Ok(c) => c,
        Err(_) => return Ok(None),
    };
    if creds.is_valid_shape() {
        Ok(Some(creds))
    } else {
        Ok(None)
    }
}
