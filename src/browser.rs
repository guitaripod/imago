//! Derive an Instagram session straight from a logged-in browser.
//!
//! Chromium browsers store cookies AES-128-CBC encrypted under a key kept in the
//! OS keyring (Linux `v11` / Secret Service, macOS `v10` / Keychain) or the
//! hardcoded "peanuts" password (`v10`, headless / `--password-store=basic`).
//! This reads the `sessionid` + `csrftoken` cookies for `.instagram.com` and
//! decrypts them so `imago auth login` needs no manual devtools copy-paste.

use std::path::{Path, PathBuf};

use aes::Aes128;
use cbc::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
use hmac::Hmac;
use rusqlite::{Connection, OpenFlags};
use sha1::Sha1;

use crate::auth::Credentials;
use crate::error::{ImagoError, Result};

type Aes128CbcDec = cbc::Decryptor<Aes128>;

const COOKIES_PATH_ENV: &str = "IMAGO_COOKIES_PATH";
const BROWSER_ENV: &str = "IMAGO_BROWSER";
const PBKDF2_SALT: &[u8] = b"saltysalt";
const KEY_LEN: usize = 16;
const AES_IV: [u8; 16] = [0x20; 16];
const INTEGRITY_PREFIX_LEN: usize = 32;

pub struct Browser {
    pub label: &'static str,
    linux_roots: &'static [&'static str],
    macos_roots: &'static [&'static str],
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    macos_keychain_service: &'static str,
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    macos_keychain_account: &'static str,
}

const BROWSERS: &[Browser] = &[
    Browser {
        label: "Vivaldi",
        linux_roots: &["vivaldi", "vivaldi-snapshot"],
        macos_roots: &["Vivaldi", "Vivaldi Snapshot"],
        macos_keychain_service: "Vivaldi Safe Storage",
        macos_keychain_account: "Vivaldi",
    },
    Browser {
        label: "Chrome",
        linux_roots: &[
            "google-chrome",
            "google-chrome-beta",
            "google-chrome-unstable",
        ],
        macos_roots: &[
            "Google/Chrome",
            "Google/Chrome Beta",
            "Google/Chrome Dev",
            "Google/Chrome Canary",
        ],
        macos_keychain_service: "Chrome Safe Storage",
        macos_keychain_account: "Chrome",
    },
    Browser {
        label: "Chromium",
        linux_roots: &["chromium"],
        macos_roots: &["Chromium"],
        macos_keychain_service: "Chromium Safe Storage",
        macos_keychain_account: "Chromium",
    },
    Browser {
        label: "Brave",
        linux_roots: &[
            "BraveSoftware/Brave-Browser",
            "BraveSoftware/Brave-Browser-Beta",
            "BraveSoftware/Brave-Browser-Nightly",
        ],
        macos_roots: &[
            "BraveSoftware/Brave-Browser",
            "BraveSoftware/Brave-Browser-Beta",
            "BraveSoftware/Brave-Browser-Nightly",
        ],
        macos_keychain_service: "Brave Safe Storage",
        macos_keychain_account: "Brave",
    },
    Browser {
        label: "Microsoft Edge",
        linux_roots: &[
            "microsoft-edge",
            "microsoft-edge-beta",
            "microsoft-edge-dev",
            "microsoft-edge-canary",
        ],
        macos_roots: &[
            "Microsoft Edge",
            "Microsoft Edge Beta",
            "Microsoft Edge Dev",
            "Microsoft Edge Canary",
        ],
        macos_keychain_service: "Microsoft Edge Safe Storage",
        macos_keychain_account: "Microsoft Edge",
    },
    Browser {
        label: "Opera",
        linux_roots: &["opera"],
        macos_roots: &[
            "com.operasoftware.Opera",
            "com.operasoftware.OperaDeveloper",
            "com.operasoftware.OperaNext",
        ],
        macos_keychain_service: "Opera Safe Storage",
        macos_keychain_account: "Opera",
    },
];

pub fn browser_labels() -> Vec<&'static str> {
    BROWSERS.iter().map(|b| b.label).collect()
}

struct Candidate {
    browser: &'static Browser,
    path: PathBuf,
}

/// Read the logged-in Instagram session from a browser cookie store.
/// `pin` (CLI flag / `IMAGO_BROWSER`) restricts extraction to one browser.
pub async fn extract(pin: Option<&str>) -> Result<Credentials> {
    let pin = pin
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::env::var(BROWSER_ENV)
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        });

    if let Some(p) = &pin {
        if !BROWSERS.iter().any(|b| b.label.eq_ignore_ascii_case(p)) {
            return Err(ImagoError::Usage(format!(
                "unknown browser \"{p}\" — choose one of: {}",
                browser_labels().join(", ")
            )));
        }
    }

    let mut found_cookies: Option<&'static str> = None;
    for Candidate { browser, path } in candidate_paths()? {
        if let Some(p) = &pin {
            if !browser.label.eq_ignore_ascii_case(p) {
                continue;
            }
        }
        if !path.exists() {
            continue;
        }
        let rows = match read_encrypted_cookies(&path) {
            Ok(rows) if has_session(&rows) => rows,
            Ok(_) => continue,
            Err(e) => {
                tracing::debug!("{} cookies unreadable: {e}", browser.label);
                continue;
            }
        };
        found_cookies = Some(browser.label);
        let passwords = backend::candidate_passwords(browser).await;
        if passwords.is_empty() {
            tracing::debug!("{} has no candidate keyring passwords", browser.label);
            continue;
        }
        match decrypt_session(&rows, &passwords) {
            Ok(mut creds) => {
                creds.user_agent = None;
                tracing::debug!("extracted Instagram session from {}", browser.label);
                return Ok(creds);
            }
            Err(e) => tracing::debug!("{} decrypt failed: {e}", browser.label),
        }
    }

    match found_cookies {
        Some(b) => Err(ImagoError::Auth(format!(
            "found an Instagram session in {b} but could not decrypt it \
             (keyring locked?) — unlock your keyring or pass --session-id/--csrf-token"
        ))),
        None => Err(ImagoError::Auth(format!(
            "no logged-in Instagram session found in any supported browser ({}). \
             Log into instagram.com in your browser, or pass --session-id/--csrf-token",
            browser_labels().join(", ")
        ))),
    }
}

fn candidate_paths() -> Result<Vec<Candidate>> {
    if let Some(override_path) = std::env::var_os(COOKIES_PATH_ENV) {
        let path = PathBuf::from(override_path);
        return Ok(vec![Candidate {
            browser: infer_browser(&path),
            path,
        }]);
    }
    let config = dirs::config_dir().ok_or_else(|| ImagoError::Other("no config dir".into()))?;
    let mut out = Vec::new();
    for browser in BROWSERS {
        for root in browser_roots(browser) {
            let root_path = config.join(root);
            if !root_path.exists() {
                continue;
            }
            for profile in profile_dirs(&root_path) {
                for rel in ["Network/Cookies", "Cookies"] {
                    out.push(Candidate {
                        browser,
                        path: profile.join(rel),
                    });
                }
            }
        }
    }
    Ok(out)
}

fn infer_browser(path: &Path) -> &'static Browser {
    let s = path.to_string_lossy();
    for browser in BROWSERS {
        for root in browser.linux_roots.iter().chain(browser.macos_roots.iter()) {
            if s.contains(root) {
                return browser;
            }
        }
    }
    &BROWSERS[0]
}

fn browser_roots(browser: &Browser) -> &'static [&'static str] {
    #[cfg(target_os = "macos")]
    {
        browser.macos_roots
    }
    #[cfg(not(target_os = "macos"))]
    {
        browser.linux_roots
    }
}

fn profile_dirs(root: &Path) -> Vec<PathBuf> {
    let mut out = vec![root.to_path_buf(), root.join("Default")];
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                if name.starts_with("Profile ") {
                    out.push(entry.path());
                }
            }
        }
    }
    out
}

fn has_session(rows: &[(String, Vec<u8>)]) -> bool {
    let names: Vec<&str> = rows.iter().map(|(n, _)| n.as_str()).collect();
    names.contains(&"sessionid") && names.contains(&"csrftoken")
}

fn read_encrypted_cookies(path: &Path) -> Result<Vec<(String, Vec<u8>)>> {
    let tmp = tempfile::NamedTempFile::new()?;
    std::fs::copy(path, tmp.path())?;
    let conn = Connection::open_with_flags(
        tmp.path(),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| ImagoError::Other(e.to_string()))?;
    let mut stmt = conn
        .prepare(
            "SELECT name, encrypted_value FROM cookies \
             WHERE host_key = '.instagram.com' AND name IN ('sessionid', 'csrftoken')",
        )
        .map_err(|e| ImagoError::Other(e.to_string()))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
        })
        .map_err(|e| ImagoError::Other(e.to_string()))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| ImagoError::Other(e.to_string()))?);
    }
    Ok(out)
}

fn decrypt_session(rows: &[(String, Vec<u8>)], passwords: &[Vec<u8>]) -> Result<Credentials> {
    let mut winning_key: Option<[u8; KEY_LEN]> = None;
    let mut session_id = None;
    let mut csrf_token = None;
    for (name, ciphertext) in rows {
        let reused = winning_key.and_then(|key| decrypt_value(ciphertext, &key).ok());
        let plain = match reused {
            Some(plain) => plain,
            None => {
                let (key, plain) = try_all(ciphertext, passwords)?;
                winning_key = Some(key);
                plain
            }
        };
        match name.as_str() {
            "sessionid" => session_id = Some(plain),
            "csrftoken" => csrf_token = Some(plain),
            _ => {}
        }
    }
    match (session_id, csrf_token) {
        (Some(session_id), Some(csrf_token)) => Ok(Credentials {
            session_id,
            csrf_token,
            user_agent: None,
        }),
        _ => Err(ImagoError::Auth(
            "cookie store missing sessionid/csrftoken".into(),
        )),
    }
}

fn derive_key(password: &[u8]) -> Option<[u8; KEY_LEN]> {
    let mut key = [0u8; KEY_LEN];
    pbkdf2::pbkdf2::<Hmac<Sha1>>(password, PBKDF2_SALT, backend::ITERS, &mut key).ok()?;
    Some(key)
}

fn try_all(encrypted: &[u8], passwords: &[Vec<u8>]) -> Result<([u8; KEY_LEN], String)> {
    let mut last = ImagoError::Auth("no candidate key decrypted the cookie".into());
    for password in passwords {
        let Some(key) = derive_key(password) else {
            continue;
        };
        match decrypt_value(encrypted, &key) {
            Ok(text) => return Ok((key, text)),
            Err(e) => last = e,
        }
    }
    Err(last)
}

fn decrypt_value(encrypted: &[u8], key: &[u8; KEY_LEN]) -> Result<String> {
    let plain = decrypt_aes_cbc(encrypted, key)?;
    if !plain.is_empty() && is_printable(&plain) {
        return Ok(String::from_utf8_lossy(&plain).into_owned());
    }
    if plain.len() > INTEGRITY_PREFIX_LEN && is_printable(&plain[INTEGRITY_PREFIX_LEN..]) {
        return Ok(String::from_utf8_lossy(&plain[INTEGRITY_PREFIX_LEN..]).into_owned());
    }
    Err(ImagoError::Auth(
        "decrypted bytes are not a printable cookie".into(),
    ))
}

fn decrypt_aes_cbc(encrypted: &[u8], key: &[u8; KEY_LEN]) -> Result<Vec<u8>> {
    let body = backend::PREFIXES
        .iter()
        .find_map(|p| encrypted.strip_prefix(*p))
        .ok_or_else(|| ImagoError::Auth("cookie missing version prefix".into()))?;
    if body.is_empty() || body.len() % 16 != 0 {
        return Err(ImagoError::Auth(
            "ciphertext length not a multiple of 16".into(),
        ));
    }
    let mut buf = body.to_vec();
    let plain = Aes128CbcDec::new(key.into(), &AES_IV.into())
        .decrypt_padded_mut::<Pkcs7>(&mut buf)
        .map_err(|_| ImagoError::Auth("AES-CBC/PKCS7 decrypt failed".into()))?;
    Ok(plain.to_vec())
}

fn is_printable(bytes: &[u8]) -> bool {
    bytes
        .iter()
        .all(|b| *b == b'\t' || *b == b'\n' || *b == b'\r' || (*b >= 0x20 && *b < 0x7f))
}

#[cfg(target_os = "linux")]
mod backend {
    use super::Browser;

    pub const PREFIXES: &[&[u8]] = &[b"v10", b"v11"];
    pub const ITERS: u32 = 1;
    const PEANUTS: &[u8] = b"peanuts";

    pub async fn candidate_passwords(_browser: &Browser) -> Vec<Vec<u8>> {
        let mut out = collect_keyring_secrets().await.unwrap_or_default();
        out.push(PEANUTS.to_vec());
        out
    }

    /// Every "Safe Storage" and `xdg-desktop-portal` secret in the login keyring
    /// is a candidate — browsers cross-name their keys (Vivaldi persists under
    /// "Chrome Safe Storage", KDE routes through the desktop portal), so pulling
    /// them all and trying each is more robust than guessing one label.
    async fn collect_keyring_secrets() -> Option<Vec<Vec<u8>>> {
        use secret_service::{EncryptionType, SecretService};
        let ss = SecretService::connect(EncryptionType::Plain).await.ok()?;
        let collection = ss.get_default_collection().await.ok()?;
        let _ = collection.unlock().await;
        let items = collection.get_all_items().await.ok()?;
        let labels = futures::future::join_all(items.iter().map(|i| i.get_label())).await;
        let mut out = Vec::new();
        for (item, label) in items.iter().zip(labels) {
            let label = label.unwrap_or_default();
            if !label.contains("Safe Storage") && !label.starts_with("xdg-desktop-portal") {
                continue;
            }
            if let Ok(secret) = item.get_secret().await {
                if !secret.is_empty() {
                    out.push(secret);
                }
            }
        }
        Some(out)
    }
}

#[cfg(target_os = "macos")]
mod backend {
    use super::Browser;

    pub const PREFIXES: &[&[u8]] = &[b"v10"];
    pub const ITERS: u32 = 1003;

    pub async fn candidate_passwords(browser: &Browser) -> Vec<Vec<u8>> {
        match security_framework::passwords::get_generic_password(
            browser.macos_keychain_service,
            browser.macos_keychain_account,
        ) {
            Ok(pw) if !pw.is_empty() => vec![pw],
            _ => Vec::new(),
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod backend {
    use super::Browser;

    pub const PREFIXES: &[&[u8]] = &[b"v10", b"v11"];
    pub const ITERS: u32 = 1;

    pub async fn candidate_passwords(_browser: &Browser) -> Vec<Vec<u8>> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cbc::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};

    type Aes128CbcEnc = cbc::Encryptor<Aes128>;

    fn encrypt(prefix: &[u8], password: &[u8], plaintext: &[u8]) -> Vec<u8> {
        let key = derive_key(password).unwrap();
        let ct = Aes128CbcEnc::new(&key.into(), &AES_IV.into())
            .encrypt_padded_vec_mut::<Pkcs7>(plaintext);
        let mut out = prefix.to_vec();
        out.extend_from_slice(&ct);
        out
    }

    #[test]
    fn infers_browser_from_override_path() {
        assert_eq!(
            infer_browser(Path::new("/home/a/.config/vivaldi/Default/Cookies")).label,
            "Vivaldi"
        );
        assert_eq!(
            infer_browser(Path::new("/home/a/.config/google-chrome/Default/Cookies")).label,
            "Chrome"
        );
        assert_eq!(
            infer_browser(Path::new("/tmp/random.db")).label,
            BROWSERS[0].label
        );
    }

    #[test]
    fn has_session_needs_both_cookies() {
        let both = vec![("sessionid".into(), vec![]), ("csrftoken".into(), vec![])];
        assert!(has_session(&both));
        assert!(!has_session(&[("sessionid".into(), vec![])]));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn decrypts_v10_and_v11_session() {
        let keyring = b"keyring-secret".to_vec();
        let rows = vec![
            (
                "sessionid".to_string(),
                encrypt(b"v11", &keyring, b"192008031%3Atoken"),
            ),
            (
                "csrftoken".to_string(),
                encrypt(b"v10", b"peanuts", b"abc123"),
            ),
        ];
        let creds = decrypt_session(&rows, &[keyring, b"peanuts".to_vec()]).unwrap();
        assert_eq!(creds.session_id, "192008031%3Atoken");
        assert_eq!(creds.csrf_token, "abc123");
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn strips_32_byte_integrity_prefix() {
        let pw = b"k".to_vec();
        let mut value = vec![0u8; INTEGRITY_PREFIX_LEN];
        value.extend_from_slice(b"realcsrf");
        let blob = encrypt(b"v10", &pw, &value);
        let (_, text) = try_all(&blob, &[pw]).unwrap();
        assert_eq!(text, "realcsrf");
    }
}
