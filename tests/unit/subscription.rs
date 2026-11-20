use super::*;
use crate::test_fixtures;

#[test]
fn imports_only_concrete_supported_nodes_in_order() {
    let yaml = br#"
proxies:
  - {name: "TCP", type: vless, server: node.test, port: "443", uuid: "d3bd732d-0c6e-4b39-ac70-be96842fc0c0", tls: true, flow: xtls-rprx-vision, servername: sni.test, client-fingerprint: firefox, reality-opts: {public-key: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", short-id: "0022"}}
  - {name: "Skip", type: direct, server: host, port: 1}
  - {name: "QUIC", type: hysteria2, server: quic.test, port: 443, password: example, sni: quic.test, alpn: [h3], obfs: salamander, obfs-password: sample, up: 10, down: "20 Mbps"}
rules: [MATCH,DIRECT]
"#;
    let result = parse(yaml).unwrap();
    assert_eq!(result.nodes.len(), 2);
    assert_eq!(result.skipped.len(), 1);
    let reality = &result.nodes[0].outbound["streamSettings"]["realitySettings"];
    assert_eq!(reality["shortId"], "0022");
    assert_eq!(reality["fingerprint"], "firefox");
    let transport = &result.nodes[1].outbound["streamSettings"];
    assert_eq!(transport["tlsSettings"]["alpn"], json!(["h3"]));
    assert_eq!(transport["hysteriaSettings"]["auth"], "example");
    assert_eq!(
        transport["finalmask"]["udp"][0]["settings"]["password"],
        "sample"
    );
    assert_eq!(transport["finalmask"]["quicParams"]["brutalUp"], "10 Mbps");
}

#[test]
fn hysteria_client_fingerprint_does_not_override_certificate_pinning() {
    let pin = "ac".repeat(32);
    for protocol in ["hysteria2", "hy2"] {
        let input = json!({"proxies":[{"name":"QUIC","server":"node.test","port":443,
            "type":protocol,"password":"example","client-fingerprint":"chrome","fingerprint":pin}]});
        let parsed = parse(&serde_json::to_vec(&input).unwrap()).unwrap();
        assert!(parsed.skipped.is_empty());
        let tls = &parsed.nodes[0].outbound["streamSettings"]["tlsSettings"];
        assert_eq!(tls["pinnedPeerCertSha256"], pin);
        assert!(tls.get("fingerprint").is_none());
    }
}

#[test]
fn saved_skip_reasons_follow_the_display_language() -> Result<()> {
    use crate::storage::{Paths, State};

    let subscription = parse(b"proxies: [{name: hidden, type: direct, port: 123}]")?;
    let mut snapshot: State = serde_json::from_value(json!({"skipped": ["legacy detail"]}))?;
    snapshot.skipped.extend(subscription.skipped);
    let directory = tempfile::tempdir()?;
    let paths = Paths::resolve(Some(directory.path().into()))?;
    paths.save_state(&mut snapshot)?;
    let saved = paths.read_snapshot()?.skipped;
    assert_eq!(saved[0].text(Language::En), "legacy detail");
    for language in [Language::En, Language::Ru] {
        assert_eq!(
            saved[1].text(language),
            test_fixtures::messages(language)["missing_server"]
        );
    }
    Ok(())
}

#[test]
fn incompatible_nodes_and_malformed_yaml_do_not_disclose_values() {
    for extra in [
        "skip-cert-verify: true",
        "certificate: secret",
        "dialer-proxy: secret",
        "obfs: invalid",
        "ports: 443-8443",
    ] {
        let bytes = format!(
            "proxies: [{{name: secret, type: hysteria2, server: quic.test, port: 443, password: secret, {extra}}}]"
        );
        let result = parse(bytes.as_bytes()).unwrap();
        assert!(result.nodes.is_empty());
        assert_eq!(result.skipped.len(), 1);
        assert!(
            !serde_json::to_string(&result.skipped)
                .unwrap()
                .contains("secret")
        );
    }
    assert!(parse(b"proxies: []").unwrap().nodes.is_empty());
    let error = parse(b"proxies: [{{secret").err().unwrap().to_string();
    assert!(!error.contains("secret"));
}
