use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

/// `HMAC-SHA256(key, salt || signed_path)`, matching imgproxy's own
/// signature input exactly (`salt` and `signed_path` concatenated with no
/// separator, in that order).
fn compute_signature(key: &[u8], salt: &[u8], signed_path: &str) -> [u8; 32] {
    // `Hmac::new_from_slice` only fails for a key length its internal block
    // cipher rejects, which never happens for HMAC (any length key is
    // valid - short keys are zero-padded, long ones are pre-hashed).
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC-SHA256 accepts any key length");
    mac.update(salt);
    mac.update(signed_path.as_bytes());
    mac.finalize().into_bytes().into()
}

/// Verifies `provided_signature` (the URL's first path segment,
/// base64url-encoded with no padding, exactly as imgproxy encodes its own
/// signatures) against `salt || signed_path` under `key`.
///
/// Constant-time by construction: the only data-dependent branch below is
/// the decoded-length check, which leaks nothing secret (signature length
/// isn't a secret - only its *content* is, and that comparison goes through
/// [`ConstantTimeEq`]). Both real imgproxy and every other established HMAC
/// verifier compare lengths in the clear for the same reason.
pub fn verify_signature(key: &[u8], salt: &[u8], signed_path: &str, provided_signature: &str) -> bool {
    let Ok(provided) = URL_SAFE_NO_PAD.decode(provided_signature) else {
        return false;
    };

    let expected = compute_signature(key, salt, signed_path);

    if provided.len() != expected.len() {
        return false;
    }

    provided.ct_eq(&expected).into()
}

/// Computes the signature for a path — the counterpart to
/// [`verify_signature`].
///
/// Deliberately **not** `#[cfg(test)]`: generating a signed URL is a real
/// capability, not a test fixture. Any client integrating with this service
/// has to produce exactly this value, `src/bin/benchmark.rs` needs it to
/// exercise the verification path rather than only the `unsigned` escape,
/// and the handler tests use it to reach the accept path.
pub fn sign(key: &[u8], salt: &[u8], signed_path: &str) -> String {
    URL_SAFE_NO_PAD.encode(compute_signature(key, salt, signed_path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_signature_is_accepted() {
        let key = b"my-signing-key";
        let salt = b"my-salt";
        let signed_path = "/rs:fill:300:300/aHR0cHM6Ly9leGFtcGxlLmNvbS9pbWcuanBn.png";

        let signature = sign(key, salt, signed_path);
        assert!(verify_signature(key, salt, signed_path, &signature));
    }

    #[test]
    fn tampered_path_is_rejected() {
        let key = b"my-signing-key";
        let salt = b"my-salt";
        let signed_path = "/rs:fill:300:300/aHR0cHM6Ly9leGFtcGxlLmNvbS9pbWcuanBn.png";
        let signature = sign(key, salt, signed_path);

        let tampered_path = "/rs:fill:999:999/aHR0cHM6Ly9leGFtcGxlLmNvbS9pbWcuanBn.png";
        assert!(!verify_signature(key, salt, tampered_path, &signature));
    }

    #[test]
    fn tampered_signature_is_rejected() {
        let key = b"my-signing-key";
        let salt = b"my-salt";
        let signed_path = "/q:80/aHR0cHM6Ly9leGFtcGxlLmNvbS9pbWcuanBn.jpg";
        let mut signature = sign(key, salt, signed_path);
        // Flip the last character - still valid base64url, still wrong.
        signature.replace_range(signature.len() - 1.., if signature.ends_with('A') { "B" } else { "A" });

        assert!(!verify_signature(key, salt, signed_path, &signature));
    }

    #[test]
    fn wrong_key_is_rejected() {
        let salt = b"my-salt";
        let signed_path = "/bl:5/aHR0cHM6Ly9leGFtcGxlLmNvbS9pbWcuanBn.webp";
        let signature = sign(b"correct-key", salt, signed_path);

        assert!(!verify_signature(b"wrong-key", salt, signed_path, &signature));
    }

    #[test]
    fn wrong_salt_is_rejected() {
        let key = b"my-signing-key";
        let signed_path = "/g:true/aHR0cHM6Ly9leGFtcGxlLmNvbS9pbWcuanBn.jpg";
        let signature = sign(key, b"correct-salt", signed_path);

        assert!(!verify_signature(key, b"wrong-salt", signed_path, &signature));
    }

    #[test]
    fn malformed_base64_signature_is_rejected() {
        let key = b"my-signing-key";
        let salt = b"my-salt";
        let signed_path = "/el:1/aHR0cHM6Ly9leGFtcGxlLmNvbS9pbWcuanBn.png";

        assert!(!verify_signature(key, salt, signed_path, "not valid base64!!"));
    }

    #[test]
    fn empty_signature_is_rejected() {
        let key = b"my-signing-key";
        let salt = b"my-salt";
        let signed_path = "/q:50/aHR0cHM6Ly9leGFtcGxlLmNvbS9pbWcuanBn.png";

        assert!(!verify_signature(key, salt, signed_path, ""));
    }
}

/// Pins the exact worked example quoted in `README.md` / `docs/user-guide/api-reference.md`
/// (`SIGNING_KEY=6d792d7369676e696e672d6b6579`, `SIGNING_SALT=6d792d73616c74`)
/// so those docs can never silently drift from what this module actually
/// computes.
#[cfg(test)]
mod documented_example {
    use super::*;

    fn hex_decode(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn docs_worked_example_signature_is_correct() {
        let key = hex_decode("6d792d7369676e696e672d6b6579");
        let salt = hex_decode("6d792d73616c74");
        let signed_path = "/rs:fill:300:300/q:80/aHR0cHM6Ly9pbWFnZXMuZXhhbXBsZS5jb20vcGhvdG8uanBn.jpg";
        let expected_sig = "de7BKgwO8wFeNZWRWgp3UB9jKwOkVoYM_eMKau2ECgw";
        assert!(verify_signature(&key, &salt, signed_path, expected_sig));
    }
}
