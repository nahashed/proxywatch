use crate::{profile::validate_outbound, runtime_text::message, subscription::Node};
use anyhow::{Result, ensure};
use serde_json::{Value, json};

pub fn private_config(
    node: &Node,
    http_port: Option<u16>,
    socks_port: Option<u16>,
) -> Result<Value> {
    ensure!(
        http_port != Some(0) && socks_port != Some(0) && http_port != socks_port,
        message!(distinct_local_ports)
    );
    validate_outbound(&node.outbound)?;
    let mut outbound = node.outbound.clone();
    outbound["tag"] = json!("selected");
    let mut inbounds = Vec::new();
    let mut tags = Vec::new();
    if let Some(port) = http_port {
        tags.push("http");
        inbounds.push(json!({"tag": "http", "listen": "127.0.0.1", "port": port, "protocol": "http", "settings": {}}));
    }
    if let Some(port) = socks_port {
        tags.push("socks");
        inbounds.push(json!({"tag": "socks", "listen": "127.0.0.1", "port": port, "protocol": "socks", "settings": {"auth":"noauth", "udp": false}}));
    }
    Ok(json!({
        "log": {"loglevel": "none"},
        "inbounds": inbounds,
        "outbounds": [outbound],
        "routing": {"rules": [{"type":"field", "inboundTag":tags, "outboundTag":"selected"}]}
    }))
}

#[cfg(test)]
#[path = "../tests/unit/config.rs"]
mod tests;
