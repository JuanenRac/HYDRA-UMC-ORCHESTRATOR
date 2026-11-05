// =============================================================================
// HYDRA-UMC-ORCHESTRATOR - src/server_identity.rs
// Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
// GPL-3.0-or-later - see LICENSE
// =============================================================================
//! The signed token of HYDRA-UMC-SERVER as the identity of a caller.
//!
//! HYDRA-UMC-SERVER is the one issuer of identity of the family. A service written in Rust verifies the token that the server issues at login: an HS256 JWT
//! with the claims `username`, `role` (`admin`, `operator` or `viewer`) and `exp`. It is a switch and it is off: nothing is asked unless the operator gives
//! the signing key in a file (`--identity-key-file`). The checks are those of the Python services (`tools/canonical/server_identity.py` of HYDRA-UMC-SDK): the
//! algorithm is exactly HS256, the signature is compared in constant time, `exp` is present, finite and in the future, `nbf` has passed, `iat` is not in the
//! future, the user name is not empty and the role is known. No dependency: the HMAC is the one of `hmac.rs`.

use serde_json::Value;

/// The shortest signing key the family accepts.
pub const MIN_KEY_LENGTH: usize = 32;
const MAX_TOKEN_CHARS: usize = 4096;
const ISSUED_SKEW_SECONDS: f64 = 5.0;

/// Who a valid token of the SERVER says is calling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    #[allow(dead_code)] // who called, for the audit of the next change
    pub username: String,
    pub role: String,
}

impl Identity {
    /// A viewer only reads; `admin` and `operator` may change something.
    pub fn can_write(&self) -> bool {
        self.role == "admin" || self.role == "operator"
    }
}

/// Reads the signing key from a file; the error never holds the key.
pub fn load_identity_key(path: &str) -> Result<Vec<u8>, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|_| "cannot read the identity key file".to_string())?;
    let key = raw.trim();
    if key.len() < MIN_KEY_LENGTH {
        return Err(format!(
            "the identity key must have at least {MIN_KEY_LENGTH} characters"
        ));
    }
    Ok(key.as_bytes().to_vec())
}

fn base64url_decode(segment: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(segment.len() * 3 / 4);
    let mut buffer: u32 = 0;
    let mut bits = 0u32;
    for byte in segment.trim_end_matches('=').bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        };
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buffer >> bits) & 0xff) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Some(out)
}

fn json_part(segment: &str) -> Result<Value, &'static str> {
    let data = base64url_decode(segment).ok_or("the token is malformed")?;
    match serde_json::from_slice::<Value>(&data) {
        Ok(value) if value.is_object() => Ok(value),
        _ => Err("the token is malformed"),
    }
}

/// A claim that, when present, must be a finite number of seconds: `Ok(None)` when absent, `Err(())` when present but not a number.
fn seconds(claims: &Value, name: &str) -> Result<Option<f64>, ()> {
    match claims.get(name) {
        None => Ok(None),
        Some(value) => match value.as_f64() {
            Some(number) if number.is_finite() => Ok(Some(number)),
            _ => Err(()),
        },
    }
}

/// The identity in a signed token of the SERVER, or the reason there is none. `now` is the time in seconds since the epoch. The reason names no key and no token.
pub fn verify_token(token: &str, key: &[u8], now: f64) -> Result<Identity, &'static str> {
    if token.is_empty() {
        return Err("a signed token of HYDRA-UMC-SERVER is required");
    }
    if token.len() > MAX_TOKEN_CHARS {
        return Err("the token is malformed");
    }
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 || parts.iter().any(|p| p.is_empty()) {
        return Err("the token is malformed");
    }
    let header = json_part(parts[0])?;
    if header.get("alg").and_then(Value::as_str) != Some("HS256") {
        return Err("the token algorithm is not accepted (only HS256)");
    }
    let signature = base64url_decode(parts[2]).ok_or("the token is malformed")?;
    let expected = crate::hmac::hmac_sha256(key, format!("{}.{}", parts[0], parts[1]).as_bytes());
    if !crate::hmac::constant_time_eq(&expected, &signature) {
        return Err("the token signature is not valid");
    }
    let claims = json_part(parts[1])?;
    match seconds(&claims, "exp") {
        Ok(Some(exp)) if exp > now => {}
        Ok(Some(_)) => return Err("the token has expired"),
        _ => return Err("the token has no expiry"),
    }
    match seconds(&claims, "nbf") {
        Ok(Some(nbf)) if nbf > now => return Err("the token is not valid yet"),
        Err(()) => return Err("the token is not valid yet"),
        _ => {}
    }
    match seconds(&claims, "iat") {
        Ok(Some(iat)) if iat > now + ISSUED_SKEW_SECONDS => {
            return Err("the token was issued in the future")
        }
        Err(()) => return Err("the token was issued in the future"),
        _ => {}
    }
    let username = claims
        .get("username")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if username.is_empty() {
        return Err("the token names no user");
    }
    let role = claims
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !matches!(role, "admin" | "operator" | "viewer") {
        return Err("the token carries no known role");
    }
    Ok(Identity {
        username: username.to_string(),
        role: role.to_string(),
    })
}

/// Whether a request with this method only reads.
pub fn is_read_method(method: &str) -> bool {
    matches!(method, "GET" | "HEAD" | "OPTIONS")
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &[u8] = b"0123456789abcdef0123456789abcdef";
    const NOW: f64 = 1_800_000_000.0;

    fn b64(data: &[u8]) -> String {
        const TABLE: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut out = String::new();
        for chunk in data.chunks(3) {
            let n = chunk
                .iter()
                .enumerate()
                .fold(0u32, |acc, (i, b)| acc | (u32::from(*b) << (16 - 8 * i)));
            let count = chunk.len() + 1;
            for i in 0..count {
                out.push(TABLE[((n >> (18 - 6 * i)) & 63) as usize] as char);
            }
        }
        out
    }

    fn token(key: &[u8], alg: &str, claims: &Value) -> String {
        let head = b64(format!("{{\"alg\":\"{alg}\",\"typ\":\"JWT\"}}").as_bytes());
        let body = b64(claims.to_string().as_bytes());
        let mac = crate::hmac::hmac_sha256(key, format!("{head}.{body}").as_bytes());
        format!("{head}.{body}.{}", b64(&mac))
    }

    fn claims(role: &str) -> Value {
        serde_json::json!({"username": "ana", "role": role, "exp": NOW + 600.0})
    }

    #[test]
    fn a_good_token_gives_the_user_and_the_role() {
        let identity = verify_token(&token(KEY, "HS256", &claims("operator")), KEY, NOW).unwrap();
        assert_eq!(identity.username, "ana");
        assert!(identity.can_write());
        let viewer = verify_token(&token(KEY, "HS256", &claims("viewer")), KEY, NOW).unwrap();
        assert!(!viewer.can_write());
    }

    #[test]
    fn every_defect_is_refused() {
        let other = b"another-key-of-more-than-32-characters!";
        let mut expired = claims("admin");
        expired["exp"] = serde_json::json!(NOW - 5.0);
        let mut not_yet = claims("admin");
        not_yet["nbf"] = serde_json::json!(NOW + 300.0);
        let mut future = claims("admin");
        future["iat"] = serde_json::json!(NOW + 300.0);
        let mut text_exp = claims("admin");
        text_exp["exp"] = serde_json::json!("tomorrow");
        let mut no_user = claims("admin");
        no_user["username"] = serde_json::json!("");
        let mut bad_role = claims("admin");
        bad_role["role"] = serde_json::json!("root");
        let cases = [
            token(KEY, "none", &claims("admin")),
            token(KEY, "HS512", &claims("admin")),
            token(other, "HS256", &claims("admin")),
            token(KEY, "HS256", &expired),
            token(
                KEY,
                "HS256",
                &serde_json::json!({"username": "ana", "role": "admin"}),
            ),
            token(KEY, "HS256", &text_exp),
            token(KEY, "HS256", &not_yet),
            token(KEY, "HS256", &future),
            token(KEY, "HS256", &no_user),
            token(KEY, "HS256", &bad_role),
            "abc".to_string(),
            "..".to_string(),
            String::new(),
            "a.".repeat(3000),
        ];
        for (index, case) in cases.iter().enumerate() {
            assert!(
                verify_token(case, KEY, NOW).is_err(),
                "case {index} was accepted"
            );
        }
    }

    #[test]
    fn the_reasons_name_no_secret() {
        let reason = verify_token(
            &token(
                b"another-key-of-more-than-32-characters!",
                "HS256",
                &claims("admin"),
            ),
            KEY,
            NOW,
        )
        .unwrap_err();
        assert!(!reason.contains("0123456789abcdef"));
    }

    #[test]
    fn the_key_comes_from_a_file_and_is_checked() {
        let dir = std::env::temp_dir().join(format!("hydra-identity-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let good = dir.join("good.key");
        let short = dir.join("short.key");
        std::fs::write(&good, format!("{}\n", String::from_utf8_lossy(KEY))).unwrap();
        std::fs::write(&short, "too-short").unwrap();
        assert_eq!(
            load_identity_key(good.to_str().unwrap()).unwrap(),
            KEY.to_vec()
        );
        let error = load_identity_key(short.to_str().unwrap()).unwrap_err();
        assert!(!error.contains("too-short"));
        assert!(load_identity_key(dir.join("missing.key").to_str().unwrap()).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
