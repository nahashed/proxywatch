use super::*;
use crate::subscription;

#[test]
fn xray_routes_only_the_enabled_listeners() -> Result<()> {
    let subscription = subscription::parse(
        b"proxies: [{name: fixture, type: hysteria2, server: node.test, port: 443, password: example}]",
    )?;
    for (http, socks, expected) in [
        (Some(18081), None, vec!["http"]),
        (None, Some(18082), vec!["socks"]),
        (Some(18081), Some(18082), vec!["http", "socks"]),
    ] {
        let config = private_config(&subscription.nodes[0], http, socks)?;
        let inbounds = config["inbounds"].as_array().unwrap();
        let protocols: Vec<_> = inbounds
            .iter()
            .map(|entry| entry["protocol"].as_str().unwrap())
            .collect();
        assert_eq!(protocols, expected);
        assert_eq!(config["routing"]["rules"][0]["inboundTag"], json!(expected));
    }
    assert!(private_config(&subscription.nodes[0], None, None).is_err());
    Ok(())
}
