//! HS256 JWTs (sign / verify), base64url, and the random strings auth needs:
//! refresh tokens and UUIDs.

use std::time::{SystemTime, UNIX_EPOCH};

use hmac::{Hmac, KeyInit, Mac};
use serde_json::Value;
use sha2::Sha256;

/// `{"alg":"HS256","typ":"JWT"}`, base64url.
const HEADER: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9";
/// Seconds an expired token is still accepted (clock skew).
const LEEWAY: u64 = 30;
/// API keys are valid for ten years, like Supabase's.
const KEY_LIFETIME: u64 = 10 * 365 * 24 * 3600;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JwtError {
    /// Not three parts, bad base64 or JSON, another `alg`, or a bad signature.
    Invalid,
    Expired,
}

/// The HS256 key, prepared once and cloned per token.
#[derive(Clone)]
pub struct Key(Hmac<Sha256>);

impl Key {
    pub fn new(secret: &[u8]) -> Key {
        Key(Hmac::new_from_slice(secret).expect("HMAC takes keys of any length"))
    }

    /// `header.payload.signature` for the JSON `payload`.
    pub fn sign(&self, payload: &str) -> String {
        let mut token = format!("{HEADER}.{}", encode(payload.as_bytes()));
        let mut mac = self.0.clone();
        mac.update(token.as_bytes());
        token.push('.');
        token.push_str(&encode(&mac.finalize().into_bytes()));
        token
    }

    /// The claims of a valid, unexpired HS256 token. `exp` is optional.
    pub fn verify(&self, token: &str, now: u64) -> Result<Value, JwtError> {
        let (signed, signature) = token.rsplit_once('.').ok_or(JwtError::Invalid)?;
        let (header, payload) = signed.split_once('.').ok_or(JwtError::Invalid)?;
        if payload.contains('.') {
            return Err(JwtError::Invalid);
        }
        let header: Value = decode(header).and_then(|h| serde_json::from_slice(&h).ok()).ok_or(JwtError::Invalid)?;
        if header["alg"] != "HS256" {
            return Err(JwtError::Invalid);
        }
        let signature = decode(signature).ok_or(JwtError::Invalid)?;
        let mut mac = self.0.clone();
        mac.update(signed.as_bytes());
        mac.verify_slice(&signature).map_err(|_| JwtError::Invalid)?;

        let claims: Value = decode(payload).and_then(|p| serde_json::from_slice(&p).ok()).ok_or(JwtError::Invalid)?;
        if !claims.is_object() {
            return Err(JwtError::Invalid);
        }
        match &claims["exp"] {
            Value::Null => Ok(claims),
            Value::Number(exp) => match exp.as_f64() {
                Some(exp) if exp + LEEWAY as f64 >= now as f64 => Ok(claims),
                _ => Err(JwtError::Expired),
            },
            _ => Err(JwtError::Invalid),
        }
    }
}

/// A legacy Supabase API key: `{iss, ref, role, iat, exp}`.
pub fn api_key(key: &Key, role: &str, now: u64) -> String {
    let exp = now + KEY_LIFETIME;
    key.sign(&format!(r#"{{"iss":"supabase","ref":"rulite","role":"{role}","iat":{now},"exp":{exp}}}"#))
}

/// Seconds since the Unix epoch.
pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// GoTrue's refresh token: 12 characters from `[a-z0-9]`, without modulo bias.
pub fn refresh_token() -> String {
    const ALPHABET: &[u8; 36] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut out = String::with_capacity(12);
    let mut buf = [0u8; 32];
    while out.len() < 12 {
        random(&mut buf);
        // 252 = 7 * 36: larger bytes would favour the first letters.
        for &b in buf.iter().filter(|&&b| b < 252) {
            if out.len() < 12 {
                out.push(ALPHABET[(b % 36) as usize] as char);
            }
        }
    }
    out
}

/// A random (version 4) UUID, lowercase with dashes.
pub fn uuid() -> String {
    let mut b = [0u8; 16];
    random(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..])
}

fn random(buf: &mut [u8]) {
    getrandom::fill(buf).expect("the OS random source is available");
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// base64url without padding.
pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | (b as u32) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            out.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

/// base64url without padding; `None` on any other character or length.
pub fn decode(s: &str) -> Option<Vec<u8>> {
    if s.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    for chunk in s.as_bytes().chunks(4) {
        let mut n = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            let v = B64.iter().position(|&x| x == c)? as u32;
            n |= v << (18 - 6 * i);
        }
        for i in 0..chunk.len() - 1 {
            out.push((n >> (16 - 8 * i)) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_760_000_000;

    fn key() -> Key {
        Key::new(b"0123456789abcdef0123456789abcdef")
    }

    #[test]
    fn base64url_round_trip() {
        assert_eq!(encode(br#"{"alg":"HS256","typ":"JWT"}"#), HEADER);
        for s in ["", "f", "fo", "foo", "foob", "fooba", "foobar", "\u{ff}\u{fe}?>"] {
            assert_eq!(decode(&encode(s.as_bytes())).unwrap(), s.as_bytes());
        }
        assert_eq!(encode(&[0xfb, 0xff]), "-_8");
        assert_eq!(decode("a"), None);
        assert_eq!(decode("ab+c"), None);
        assert_eq!(decode("ab=="), None);
    }

    #[test]
    fn round_trip_and_tampering() {
        let k = key();
        let token = k.sign(r#"{"sub":"u1","role":"authenticated","exp":1760000100}"#);
        assert_eq!(k.verify(&token, NOW).unwrap()["sub"], "u1");

        // Known HS256 vector (jwt.io's secret), so the signature matches other libraries.
        let io = Key::new(b"your-256-bit-secret");
        let signed = io.sign(r#"{"sub":"1234567890","name":"John Doe","iat":1516239022}"#);
        assert_eq!(signed.rsplit('.').next().unwrap(), "SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c");

        let (head, sig) = token.rsplit_once('.').unwrap();
        let other = encode(br#"{"sub":"u2","role":"authenticated","exp":1760000100}"#);
        let forged = format!("{HEADER}.{other}.{sig}");
        assert_eq!(k.verify(&forged, NOW), Err(JwtError::Invalid));
        assert_eq!(k.verify(&format!("{head}.{}", &sig[1..]), NOW), Err(JwtError::Invalid));
        assert_eq!(Key::new(b"another secret of 32 bytes......").verify(&token, NOW), Err(JwtError::Invalid));
        assert_eq!(k.verify("a.b", NOW), Err(JwtError::Invalid));
        assert_eq!(k.verify(&format!("{token}.x"), NOW), Err(JwtError::Invalid));
    }

    #[test]
    fn only_hs256() {
        let k = key();
        let payload = encode(br#"{"role":"service_role"}"#);
        let none = format!("{}.{payload}.", encode(br#"{"alg":"none","typ":"JWT"}"#));
        assert_eq!(k.verify(&none, NOW), Err(JwtError::Invalid));

        // Signed correctly with our key, but claiming HS512.
        let head = encode(br#"{"alg":"HS512","typ":"JWT"}"#);
        let mut mac = k.0.clone();
        mac.update(format!("{head}.{payload}").as_bytes());
        let hs512 = format!("{head}.{payload}.{}", encode(&mac.finalize().into_bytes()));
        assert_eq!(k.verify(&hs512, NOW), Err(JwtError::Invalid));
    }

    #[test]
    fn expiry_with_leeway() {
        let k = key();
        let token = |exp: u64| k.sign(&format!(r#"{{"exp":{exp}}}"#));
        assert!(k.verify(&token(NOW - LEEWAY), NOW).is_ok());
        assert_eq!(k.verify(&token(NOW - LEEWAY - 1), NOW), Err(JwtError::Expired));
        assert!(k.verify(&k.sign(r#"{"role":"anon"}"#), NOW).is_ok());
        assert_eq!(k.verify(&k.sign(r#"{"exp":"soon"}"#), NOW), Err(JwtError::Invalid));
        assert_eq!(k.verify(&k.sign("[1]"), NOW), Err(JwtError::Invalid));
    }

    #[test]
    fn api_keys() {
        let k = key();
        let key = api_key(&k, "anon", NOW);
        let payload = String::from_utf8(decode(key.split('.').nth(1).unwrap()).unwrap()).unwrap();
        assert!(payload.starts_with(&format!(r#"{{"iss":"supabase","ref":"rulite","role":"anon","iat":{NOW},"exp":"#)));
        let claims = k.verify(&key, NOW).unwrap();
        assert_eq!(claims["exp"].as_u64().unwrap() - claims["iat"].as_u64().unwrap(), KEY_LIFETIME);
    }

    #[test]
    fn random_strings() {
        let t = refresh_token();
        assert_eq!(t.len(), 12);
        assert!(t.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()));
        assert_ne!(t, refresh_token());

        let u = uuid();
        let parts: Vec<&str> = u.split('-').collect();
        assert_eq!(parts.iter().map(|p| p.len()).collect::<Vec<_>>(), [8, 4, 4, 4, 12]);
        assert!(parts[2].starts_with('4'));
        assert!(matches!(&parts[3][..1], "8" | "9" | "a" | "b"));
    }
}
