use crate::runtime_text::{NodeResult, node_error, require};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Map, Value};
use std::net::IpAddr;

pub fn validate_outbound(value: &Value) -> NodeResult<()> {
    let outbound = fields(value, &["protocol", "settings", "streamSettings"])?;
    match string(outbound, "protocol")? {
        "vless" => validate_vless(&value["settings"], &value["streamSettings"]),
        "hysteria" => validate_hysteria(&value["settings"], &value["streamSettings"]),
        _ => Err(node_error!(unsupported_outbound)),
    }
}

fn validate_vless(settings: &Value, stream: &Value) -> NodeResult<()> {
    fields(settings, &["vnext"])?;
    let server = single(&settings["vnext"])?;
    fields(server, &["address", "port", "users"])?;
    endpoint(server)?;
    let account = fields(single(&server["users"])?, &["id", "encryption", "flow"])?;
    require!(
        uuid::Uuid::parse_str(string(account, "id")?).is_ok(),
        invalid_uuid
    );
    require!(
        string(account, "encryption")? == "none",
        require_encryption_none
    );
    require!(
        matches!(
            string(account, "flow")?,
            "xtls-rprx-vision" | "xtls-rprx-vision-udp443"
        ),
        require_vision
    );
    fields(stream, &["network", "security", "realitySettings"])?;
    require!(
        stream["network"] == "raw" && stream["security"] == "reality",
        require_reality_tcp
    );
    let reality = fields(
        &stream["realitySettings"],
        &["serverName", "publicKey", "shortId", "fingerprint"],
    )?;
    host(string(reality, "serverName")?)?;
    require!(
        valid_reality_key(string(reality, "publicKey")?),
        invalid_public_key
    );
    if let Some(short_id) = optional(reality, "shortId")? {
        require!(
            short_id.len() <= 16
                && short_id.len().is_multiple_of(2)
                && short_id.bytes().all(|byte| byte.is_ascii_hexdigit()),
            invalid_short_id
        );
    }
    if let Some(value) = optional(reality, "fingerprint")? {
        validate_fingerprint(value)?;
    }
    Ok(())
}

fn validate_hysteria(settings: &Value, stream: &Value) -> NodeResult<()> {
    fields(settings, &["version", "address", "port"])?;
    endpoint(settings)?;
    fields(
        stream,
        &[
            "network",
            "security",
            "tlsSettings",
            "hysteriaSettings",
            "finalmask",
        ],
    )?;
    require!(
        settings["version"] == 2 && stream["network"] == "hysteria" && stream["security"] == "tls",
        require_hysteria_tls
    );
    let transport = fields(&stream["hysteriaSettings"], &["version", "auth"])?;
    require!(
        transport.get("version").and_then(Value::as_u64) == Some(2),
        require_hysteria_transport
    );
    string(transport, "auth")?;
    let tls = fields(
        &stream["tlsSettings"],
        &["serverName", "alpn", "pinnedPeerCertSha256"],
    )?;
    let name = string(tls, "serverName")?;
    require!(!name.eq_ignore_ascii_case("fromMitm"), unsupported_tls_name);
    host(name)?;
    if let Some(alpn) = tls.get("alpn") {
        require!(
            alpn.as_array().is_some_and(|items| !items.is_empty()
                && items
                    .iter()
                    .all(|item| item.as_str().is_some_and(|value| !value.is_empty()
                        && value.len() <= 255
                        && !value.chars().any(char::is_control)))),
            invalid_alpn
        );
    }
    if let Some(pin) = optional(tls, "pinnedPeerCertSha256")? {
        let pin = pin.replace(':', "");
        require!(
            pin.len() == 64 && pin.bytes().all(|byte| byte.is_ascii_hexdigit()),
            invalid_certificate_fingerprint
        );
    }
    if let Some(value) = stream.get("finalmask") {
        validate_mask(value)?;
    }
    Ok(())
}

fn validate_mask(value: &Value) -> NodeResult<()> {
    let mask = fields(value, &["udp", "quicParams"])?;
    if let Some(udp) = mask.get("udp") {
        let entry = single(udp)?;
        fields(entry, &["type", "settings"])?;
        require!(entry["type"] == "salamander", unsupported_udp_mask);
        string(fields(&entry["settings"], &["password"])?, "password")?;
    }
    if let Some(value) = mask.get("quicParams") {
        let quic = fields(
            value,
            &["congestion", "brutalUp", "brutalDown", "bbrProfile"],
        )?;
        if let Some(congestion) = optional(quic, "congestion")? {
            require!(congestion == "brutal", unsupported_congestion);
        }
        if let Some(profile) = optional(quic, "bbrProfile")? {
            require!(
                matches!(profile, "standard" | "conservative" | "aggressive"),
                invalid_bbr
            );
        }
        for name in ["brutalUp", "brutalDown"] {
            if let Some(speed) = optional(quic, name)? {
                let speed = bandwidth(speed)?;
                require!(speed == 0.0 || speed >= 65536.0, brutal_minimum);
            }
        }
    }
    Ok(())
}

fn fields<'a>(value: &'a Value, allowed: &[&str]) -> NodeResult<&'a Map<String, Value>> {
    let options = value
        .as_object()
        .ok_or_else(|| node_error!(expected_xray_object))?;
    require!(
        options.keys().all(|key| allowed.contains(&key.as_str())),
        unexpected_xray_options
    );
    Ok(options)
}

fn single(value: &Value) -> NodeResult<&Value> {
    match value.as_array().map(Vec::as_slice) {
        Some([item]) => Ok(item),
        _ => Err(node_error!(expected_one_item)),
    }
}

fn string<'a>(fields: &'a Map<String, Value>, key: &'static str) -> NodeResult<&'a str> {
    optional(fields, key)?
        .filter(|text| !text.is_empty())
        .ok_or_else(|| node_error!(missing_field, key))
}

fn optional<'a>(fields: &'a Map<String, Value>, key: &'static str) -> NodeResult<Option<&'a str>> {
    fields
        .get(key)
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| node_error!(expected_string, key))
        })
        .transpose()
}

fn endpoint(value: &Value) -> NodeResult<()> {
    host(
        value["address"]
            .as_str()
            .ok_or_else(|| node_error!(missing_node_address))?,
    )?;
    require!(
        value["port"]
            .as_u64()
            .is_some_and(|port| (1..=65535).contains(&port)),
        invalid_node_port
    );
    Ok(())
}

fn host(value: &str) -> NodeResult<()> {
    if value.parse::<IpAddr>().is_ok() {
        return Ok(());
    }
    let host = value.strip_suffix('.').unwrap_or(value);
    require!(
        !host.is_empty()
            && host.len() <= 253
            && host.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && !label.starts_with('-')
                    && !label.ends_with('-')
                    && label
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
            }),
        invalid_server_name
    );
    Ok(())
}

fn valid_reality_key(value: &str) -> bool {
    URL_SAFE_NO_PAD.decode_slice(value, &mut [0; 32]) == Ok(32)
}

fn bandwidth(value: &str) -> NodeResult<f64> {
    let value = value.trim().to_ascii_lowercase();
    if value.is_empty() {
        return Ok(0.0);
    }
    let end = value
        .find(|character: char| !character.is_ascii_digit() && character != '.')
        .unwrap_or(value.len());
    let number: f64 = value[..end]
        .parse()
        .map_err(|_| node_error!(invalid_quic_rate))?;
    let factor = match value[end..].trim() {
        "" | "b" | "bps" => 1.0,
        "k" | "kb" | "kbps" => 1024.0,
        "m" | "mb" | "mbps" => 1024.0 * 1024.0,
        "g" | "gb" | "gbps" => 1024.0 * 1024.0 * 1024.0,
        "t" | "tb" | "tbps" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return Err(node_error!(unknown_quic_unit)),
    };
    let speed = number * factor / 8.0;
    require!(
        speed.is_finite() && speed >= 0.0 && speed <= u64::MAX as f64,
        invalid_quic_rate
    );
    Ok(speed)
}

fn validate_fingerprint(value: &str) -> NodeResult<()> {
    // Xray's REALITY client uses the TLS registry except for unsafe and hellogolang.
    // An empty fingerprint selects Xray's default, independently of the named presets.
    const NAMES: &[&str] = &include!("../data/xray-v26.7.28-reality-fingerprints.ron");
    require!(
        value.is_empty() || NAMES.iter().any(|name| name.eq_ignore_ascii_case(value)),
        unknown_fingerprint
    );
    Ok(())
}

#[cfg(test)]
#[path = "../tests/unit/profile.rs"]
mod tests;
