use crate::{profile::validate_outbound, subscription::Node};
use anyhow::{Result, ensure};
use serde_json::{Value, json};

pub fn private_config(node: &Node, http_port: u16, socks_port: u16) -> Result<Value> {
    ensure!(
        http_port != 0 && socks_port != 0 && http_port != socks_port,
        "Требуются два разных локальных порта"
    );
    validate_outbound(&node.outbound)?;
    let mut outbound = node.outbound.clone();
    outbound["tag"] = json!("selected");
    Ok(json!({
        "log": {"loglevel": "none"},
        "inbounds": [
            {"tag": "http", "listen": "127.0.0.1", "port": http_port, "protocol": "http", "settings": {}},
            {"tag": "socks", "listen": "127.0.0.1", "port": socks_port, "protocol": "socks", "settings": {"auth":"noauth", "udp": false}}
        ],
        "outbounds": [outbound],
        "routing": {"rules": [{"type":"field", "inboundTag":["http", "socks"], "outboundTag":"selected"}]}
    }))
}
