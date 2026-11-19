use crate::i18n::Language;
use crate::profile::validate_outbound;
use crate::runtime_text::{self, NodeError, NodeResult, message, node_error, require};
use anyhow::{Context, Result, ensure};
use reqwest::{Url, blocking::Client};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{collections::HashSet, io::Read, time::Duration};

const MAX_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub name: String,
    pub outbound: Value,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Subscription {
    pub nodes: Vec<Node>,
    pub skipped: Vec<SkippedNode>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SkippedNode {
    Entry { index: usize, reason: NodeError },
    Legacy(String),
}

impl SkippedNode {
    pub fn text(&self, language: Language) -> String {
        match self {
            Self::Entry { index, reason } => match language {
                Language::En => format!(runtime_text::en::text!(skipped_node), index, reason.en),
                Language::Ru => format!(runtime_text::ru::text!(skipped_node), index, reason.ru),
            },
            Self::Legacy(text) => text.clone(),
        }
    }
}

pub fn subscription_url(address: &str, hwid: bool) -> Result<Url> {
    let url =
        Url::parse(address).map_err(|_| anyhow::anyhow!(message!(invalid_subscription_url)))?;
    ensure!(
        matches!(url.scheme(), "https" | "http")
            && url.has_host()
            && url.username().is_empty()
            && url.password().is_none(),
        message!(subscription_url_credentials)
    );
    ensure!(!hwid || url.scheme() == "https", message!(hwid_https));
    Ok(url)
}

pub fn fetch(address: &str, hwid: bool) -> Result<Subscription> {
    let url = subscription_url(address, hwid)?;
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(8))
        .timeout(Duration::from_secs(20))
        .user_agent("Clash.Meta/1.19 Proxywatch/0.1")
        .referer(false)
        .redirect(crate::subscription_hwid::redirect_policy(hwid))
        .build()
        .context(message!(create_subscription_client))?;
    let request = crate::subscription_hwid::request(&client, url, hwid)?;
    let response = request
        .send()
        .map_err(|_| anyhow::anyhow!(message!(download_subscription)))?;
    ensure!(
        response.status().is_success(),
        message!(subscription_status, response.status().as_u16())
    );
    ensure!(
        response.content_length().unwrap_or(0) <= MAX_BYTES as u64,
        message!(subscription_size)
    );
    let mut bytes = Vec::new();
    response
        .take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow::anyhow!(message!(read_subscription)))?;
    parse(&bytes)
}

pub fn parse(bytes: &[u8]) -> Result<Subscription> {
    #[derive(Deserialize)]
    struct Document {
        proxies: Vec<Value>,
    }
    ensure!(bytes.len() <= MAX_BYTES, message!(subscription_size));
    let document: Document = serde_yaml_ng::from_slice(bytes)
        .map_err(|_| anyhow::anyhow!(message!(subscription_format)))?;
    ensure!(
        document.proxies.len() <= 512,
        message!(subscription_node_limit)
    );
    let mut result = Subscription {
        nodes: Vec::new(),
        skipped: Vec::new(),
    };
    let mut names = HashSet::new();
    for (index, value) in document.proxies.into_iter().enumerate() {
        let parsed = convert(&value).and_then(|node| {
            require!(names.insert(node.name.clone()), duplicate_node);
            Ok(node)
        });
        match parsed {
            Ok(node) => result.nodes.push(node),
            Err(reason) => result.skipped.push(SkippedNode::Entry {
                index: index + 1,
                reason,
            }),
        }
    }
    Ok(result)
}

fn convert(value: &Value) -> NodeResult<Node> {
    let fields = value
        .as_object()
        .ok_or_else(|| node_error!(invalid_entry))?;
    let name = text(fields, "name")?;
    require!(
        !name.trim().is_empty()
            && name.chars().count() <= 150
            && !name.chars().any(|character| character.is_control()
                || matches!(character, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')),
        invalid_node_name
    );
    let server = text(fields, "server")?;
    let port = value["port"]
        .as_u64()
        .or_else(|| value["port"].as_str()?.parse().ok())
        .ok_or_else(|| node_error!(invalid_node_port))?;
    let outbound = match text(fields, "type")? {
        "vless" => vless(fields, server, port)?,
        "hysteria2" | "hy2" => hysteria(fields, server, port)?,
        _ => return Err(node_error!(unsupported_proxy_type)),
    };
    validate_outbound(&outbound)?;
    Ok(Node {
        name: name.to_owned(),
        outbound,
    })
}

fn text<'a>(fields: &'a Map<String, Value>, key: &'static str) -> NodeResult<&'a str> {
    fields
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| node_error!(required_string, key))
}

fn known(fields: &Map<String, Value>, extra: &[&str]) -> NodeResult<()> {
    let common = [
        "name",
        "type",
        "server",
        "port",
        "udp",
        "tfo",
        "mptcp",
        "skip-cert-verify",
        "smux",
    ];
    require!(
        fields
            .keys()
            .all(|key| common.contains(&key.as_str()) || extra.contains(&key.as_str())),
        unsupported_mihomo_fields
    );
    for key in ["tfo", "mptcp", "skip-cert-verify"] {
        require!(
            fields.get(key).is_none_or(|value| value == false),
            unsupported_option,
            key
        );
    }
    if let Some(smux) = fields.get("smux") {
        require!(
            smux.is_object() && smux["enabled"] == false,
            unsupported_smux
        );
    }
    Ok(())
}

fn vless(fields: &Map<String, Value>, server: &str, port: u64) -> NodeResult<Value> {
    known(
        fields,
        &[
            "uuid",
            "tls",
            "network",
            "reality-opts",
            "encryption",
            "flow",
            "servername",
            "client-fingerprint",
            "packet-encoding",
        ],
    )?;
    require!(
        fields.get("tls").is_some_and(|value| value == true),
        require_vless_reality
    );
    require!(
        fields
            .get("network")
            .is_none_or(|value| value == "tcp" || value == "raw"),
        require_vless_tcp
    );
    require!(
        fields
            .get("encryption")
            .is_none_or(|value| value == "" || value == "none"),
        unsupported_encryption
    );
    let limits = fields
        .get("reality-opts")
        .and_then(Value::as_object)
        .ok_or_else(|| node_error!(require_reality_opts))?;
    require!(limits.keys().all(|key| ["public-key","short-id","support-x25519mlkem768"].contains(&key.as_str())), unsupported_reality_options);
    require!(
        limits.get("support-x25519mlkem768").is_none(),
        unsupported_reality_group
    );
    let mut reality = json!({"publicKey": text(limits,"public-key")?, "serverName": fields.get("servername").map_or(json!(server), Value::clone)});
    if let Some(value) = limits.get("short-id") {
        reality["shortId"] = value.clone();
    }
    if let Some(value) = fields.get("client-fingerprint") {
        reality["fingerprint"] = value.clone();
    }
    Ok(json!({
        "protocol":"vless",
        "settings":{"vnext":[{"address":server,"port":port,"users":[{"id":text(fields,"uuid")?,"flow":text(fields,"flow")?,"encryption":"none"}]}]},
        "streamSettings":{"network":"raw","security":"reality","realitySettings":reality}
    }))
}

fn hysteria(fields: &Map<String, Value>, server: &str, port: u64) -> NodeResult<Value> {
    known(
        fields,
        &[
            "password",
            "sni",
            "alpn",
            "obfs",
            "obfs-password",
            "fingerprint",
            "up",
            "down",
            "bbr-profile",
        ],
    )?;
    let mut tls = json!({"serverName": fields.get("sni").map_or(json!(server), Value::clone)});
    if let Some(value) = fields.get("alpn") {
        tls["alpn"] = value.clone();
    }
    if let Some(value) = fields.get("fingerprint") {
        tls["pinnedPeerCertSha256"] = value.clone();
    }
    let mut stream = json!({
        "network":"hysteria","security":"tls","tlsSettings":tls,
        "hysteriaSettings":{"version":2, "auth":text(fields,"password")?}
    });
    if let Some(value) = fields.get("obfs")
        && value != ""
    {
        require!(value == "salamander", unsupported_obfuscation);
        stream["finalmask"]["udp"] =
            json!([{"type":"salamander","settings":{"password":text(fields, "obfs-password")?}}]);
    } else {
        require!(
            !fields.contains_key("obfs-password"),
            missing_obfuscation_type
        );
    }
    for (source, target) in [("up", "brutalUp"), ("down", "brutalDown")] {
        if let Some(value) = fields.get(source) {
            let speed = match value {
                Value::String(text) => text.clone(),
                Value::Number(number) => number.to_string(),
                _ => return Err(node_error!(invalid_hysteria_rate)),
            };
            let speed = speed.trim();
            let speed = if speed.parse::<f64>().is_ok() {
                format!("{speed} Mbps")
            } else {
                speed.to_owned()
            };
            stream["finalmask"]["quicParams"][target] = json!(speed);
            stream["finalmask"]["quicParams"]["congestion"] = json!("brutal");
        }
    }
    if let Some(profile) = fields.get("bbr-profile") {
        stream["finalmask"]["quicParams"]["bbrProfile"] = profile.clone();
    }
    Ok(
        json!({"protocol":"hysteria","settings":{"version":2,"address":server,"port":port},"streamSettings":stream}),
    )
}

#[cfg(test)]
#[path = "../tests/unit/subscription.rs"]
mod tests;
