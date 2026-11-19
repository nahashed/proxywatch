use super::*;

#[test]
fn reality_key_requires_canonical_unpadded_base64url_for_32_bytes() {
    let key = URL_SAFE_NO_PAD.encode([0xff; 32]);
    assert!(valid_reality_key(&key));
    assert!(!valid_reality_key(&key.replace('_', "/")));
    assert!(!valid_reality_key(&format!("{key}=")));
    assert!(!valid_reality_key(&key.replace('8', "9")));
    assert!(!valid_reality_key(&URL_SAFE_NO_PAD.encode([0; 31])));
    assert!(!valid_reality_key(&URL_SAFE_NO_PAD.encode([0; 33])));
}

#[test]
fn reality_fingerprint_matches_the_supported_xray_registry() {
    for name in [
        "",
        "cHroMe",
        "randomized",
        "HelloFirefox_148",
        "helloios_11_1",
    ] {
        assert!(validate_fingerprint(name).is_ok(), "{name}");
    }
    for name in ["unsafe", "HelloGolang", "unknown", " chrome"] {
        assert!(validate_fingerprint(name).is_err(), "{name}");
    }
}
