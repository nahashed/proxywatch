use crate::profile::validate_outbound;
use anyhow::{Context, Result, bail, ensure};
use reqwest::{Url, blocking::Client, redirect::Policy};
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
    pub skipped: Vec<String>,
}

pub fn fetch(address: &str) -> Result<Subscription> {
    let url = Url::parse(address).map_err(|_| anyhow::anyhow!("Некорректный URL подписки"))?;
    ensure!(
        matches!(url.scheme(), "https" | "http")
            && url.has_host()
            && url.username().is_empty()
            && url.password().is_none(),
        "Для подписки требуется HTTP(S) URL без логина и пароля"
    );
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(8))
        .timeout(Duration::from_secs(20))
        .user_agent("Clash.Meta/1.19 Proxywatch/0.1")
        .referer(false)
        .redirect(Policy::custom(|attempt| {
            let next = attempt.url();
            if attempt.previous().len() > 4
                || !matches!(next.scheme(), "https" | "http")
                || !next.username().is_empty()
                || next.password().is_some()
                || (next.scheme() == "http"
                    && attempt.previous().iter().any(|url| url.scheme() == "https"))
            {
                attempt.stop()
            } else {
                attempt.follow()
            }
        }))
        .build()
        .context("Не удалось создать клиент подписки")?;
    let response = client
        .get(url)
        .send()
        .map_err(|_| anyhow::anyhow!("Не удалось загрузить подписку"))?;
    ensure!(
        response.status().is_success(),
        "Подписка вернула HTTP {}",
        response.status().as_u16()
    );
    ensure!(
        response.content_length().unwrap_or(0) <= MAX_BYTES as u64,
        "Подписка превышает 4 МиБ"
    );
    let mut bytes = Vec::new();
    response
        .take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow::anyhow!("Не удалось прочитать подписку"))?;
    parse(&bytes)
}

pub fn parse(bytes: &[u8]) -> Result<Subscription> {
    #[derive(Deserialize)]
    struct Document {
        proxies: Vec<Value>,
    }
    ensure!(bytes.len() <= MAX_BYTES, "Подписка превышает 4 МиБ");
    let document: Document = serde_yaml_ng::from_slice(bytes)
        .map_err(|_| anyhow::anyhow!("Подписка должна содержать Mihomo YAML со списком proxies"))?;
    ensure!(
        document.proxies.len() <= 512,
        "Подписка содержит больше 512 узлов"
    );
    let mut result = Subscription {
        nodes: Vec::new(),
        skipped: Vec::new(),
    };
    let mut names = HashSet::new();
    for (index, value) in document.proxies.into_iter().enumerate() {
        let parsed = convert(&value).and_then(|node| {
            ensure!(names.insert(node.name.clone()), "Имя узла повторяется");
            Ok(node)
        });
        match parsed {
            Ok(node) => result.nodes.push(node),
            Err(reason) => result.skipped.push(format!("Узел {}. {reason}", index + 1)),
        }
    }
    Ok(result)
}

fn convert(value: &Value) -> Result<Node> {
    let fields = value.as_object().context("Некорректная запись в proxies")?;
    let name = text(fields, "name")?;
    ensure!(
        !name.trim().is_empty()
            && name.chars().count() <= 150
            && !name.chars().any(|character| character.is_control()
                || matches!(character, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')),
        "Недопустимое имя узла"
    );
    let server = text(fields, "server")?;
    let port = value["port"]
        .as_u64()
        .or_else(|| value["port"].as_str()?.parse().ok())
        .context("Некорректный порт узла")?;
    let outbound = match text(fields, "type")? {
        "vless" => vless(fields, server, port)?,
        "hysteria2" | "hy2" => hysteria(fields, server, port)?,
        _ => bail!("Тип прокси не поддерживается в версии 0.1"),
    };
    validate_outbound(&outbound)?;
    Ok(Node {
        name: name.to_owned(),
        outbound,
    })
}

fn text<'a>(fields: &'a Map<String, Value>, key: &'static str) -> Result<&'a str> {
    fields
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .with_context(|| format!("Требуется строковое поле {key}"))
}

fn known(fields: &Map<String, Value>, extra: &[&str]) -> Result<()> {
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
    ensure!(
        fields
            .keys()
            .all(|key| common.contains(&key.as_str()) || extra.contains(&key.as_str())),
        "Профиль содержит неподдерживаемые параметры Mihomo"
    );
    for key in ["tfo", "mptcp", "skip-cert-verify"] {
        ensure!(
            fields.get(key).is_none_or(|value| value == false),
            "Параметр {key} не поддерживается"
        );
    }
    if let Some(smux) = fields.get("smux") {
        ensure!(
            smux.is_object() && smux["enabled"] == false,
            "Мультиплексирование smux не поддерживается"
        );
    }
    Ok(())
}

fn vless(fields: &Map<String, Value>, server: &str, port: u64) -> Result<Value> {
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
    ensure!(
        fields.get("tls").is_some_and(|value| value == true),
        "Требуется VLESS с REALITY"
    );
    ensure!(
        fields
            .get("network")
            .is_none_or(|value| value == "tcp" || value == "raw"),
        "Поддерживается только VLESS RAW/TCP"
    );
    ensure!(
        fields
            .get("encryption")
            .is_none_or(|value| value == "" || value == "none"),
        "Неподдерживаемое шифрование VLESS"
    );
    let limits = fields
        .get("reality-opts")
        .and_then(Value::as_object)
        .context("Требуется reality-opts")?;
    ensure!(limits.keys().all(|key| ["public-key","short-id","support-x25519mlkem768"].contains(&key.as_str())), "Неподдерживаемые параметры REALITY");
    ensure!(
        limits.get("support-x25519mlkem768").is_none(),
        "Явный выбор группы обмена ключами REALITY не поддерживается"
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

fn hysteria(fields: &Map<String, Value>, server: &str, port: u64) -> Result<Value> {
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
        ensure!(
            value == "salamander",
            "Неподдерживаемая обфускация Hysteria2"
        );
        stream["finalmask"]["udp"] =
            json!([{"type":"salamander","settings":{"password":text(fields, "obfs-password")?}}]);
    } else {
        ensure!(
            !fields.contains_key("obfs-password"),
            "Пароль обфускации указан без её типа"
        );
    }
    for (source, target) in [("up", "brutalUp"), ("down", "brutalDown")] {
        if let Some(value) = fields.get(source) {
            let speed = match value {
                Value::String(text) => text.clone(),
                Value::Number(number) => number.to_string(),
                _ => bail!("Некорректная скорость Hysteria2"),
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
mod tests {
    use super::*;

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
            assert!(!result.skipped[0].contains("secret"));
        }
        assert!(parse(b"proxies: []").unwrap().nodes.is_empty());
        let error = parse(b"proxies: [{{secret").err().unwrap().to_string();
        assert!(!error.contains("secret"));
    }
}
