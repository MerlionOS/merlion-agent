//! OAuth2 PKCE helpers + TokenStore roundtrip.
//!
//! Live OAuth-server tests are skipped — they require either a real
//! authorization server or a substantial mock of one. The deterministic
//! pieces (PKCE math, token persistence, expiry) are covered here.

use std::time::{SystemTime, UNIX_EPOCH};

use merlion_mcp::oauth::{code_challenge_s256, generate_code_verifier};
use merlion_mcp::{TokenStore, Tokens};

#[test]
fn rfc7636_appendix_b_vector() {
    // RFC 7636 Appendix B — canonical PKCE test vector.
    let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    let challenge = code_challenge_s256(verifier);
    assert_eq!(challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
}

#[test]
fn generated_verifier_is_64_chars_and_url_safe() {
    let v = generate_code_verifier();
    assert_eq!(v.len(), 64);
    for c in v.chars() {
        assert!(
            c.is_ascii_alphanumeric() || c == '-' || c == '_',
            "non-url-safe char in verifier: {c:?}"
        );
    }
}

#[test]
fn token_store_save_load_roundtrip() {
    let tmp = tempdir();
    let path = tmp.join("mcp-tokens.yaml");

    let mut store = TokenStore::default();
    store.set(
        "github".to_string(),
        Tokens {
            access_token: "tok_abc".into(),
            refresh_token: Some("rfr_xyz".into()),
            expires_at: Some(1_900_000_000),
            token_type: "Bearer".into(),
        },
    );
    store.set(
        "slack".to_string(),
        Tokens {
            access_token: "tok_slack".into(),
            refresh_token: None,
            expires_at: None,
            token_type: "Bearer".into(),
        },
    );
    store.save(&path).expect("save");

    let loaded = TokenStore::load(&path).expect("load");
    assert_eq!(loaded.servers.len(), 2);

    let gh = loaded.get("github").expect("github tokens");
    assert_eq!(gh.access_token, "tok_abc");
    assert_eq!(gh.refresh_token.as_deref(), Some("rfr_xyz"));
    assert_eq!(gh.expires_at, Some(1_900_000_000));
    assert_eq!(gh.token_type, "Bearer");

    let sl = loaded.get("slack").expect("slack tokens");
    assert_eq!(sl.access_token, "tok_slack");
    assert!(sl.refresh_token.is_none());
    assert!(sl.expires_at.is_none());
}

#[test]
fn load_missing_file_returns_empty_store() {
    let tmp = tempdir();
    let path = tmp.join("does-not-exist.yaml");
    let store = TokenStore::load(&path).expect("load missing");
    assert!(store.servers.is_empty());
}

#[test]
fn expired_token_is_recognized() {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let stale = Tokens {
        access_token: "x".into(),
        refresh_token: None,
        expires_at: Some(now - 60),
        token_type: "Bearer".into(),
    };
    assert!(stale.is_expired(), "60s-old token must be expired");

    let fresh = Tokens {
        access_token: "x".into(),
        refresh_token: None,
        expires_at: Some(now + 3600),
        token_type: "Bearer".into(),
    };
    assert!(!fresh.is_expired(), "1h-future token must not be expired");

    let unknown = Tokens {
        access_token: "x".into(),
        refresh_token: None,
        expires_at: None,
        token_type: "Bearer".into(),
    };
    assert!(
        !unknown.is_expired(),
        "token with no expires_at is treated as non-expiring"
    );
}

/// Per-process unique temp dir. We avoid pulling in the `tempfile` crate
/// just for a few tests — std::env::temp_dir + pid + nanos is plenty.
fn tempdir() -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!(
        "merlion-oauth-test-{}-{}",
        std::process::id(),
        nanos
    ));
    std::fs::create_dir_all(&path).expect("mkdir tempdir");
    path
}
