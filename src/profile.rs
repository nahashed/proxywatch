use anyhow::{Context, Result, bail, ensure};
use serde_json::{Map, Value};
use std::net::IpAddr;

pub fn validate_outbound(value: &Value) -> Result<()> {
    let outbound = fields(value, &["protocol", "settings", "streamSettings"])?;
    match string(outbound, "protocol")? {
        "vless" => validate_vless(&value["settings"], &value["streamSettings"]),
        "hysteria" => validate_hysteria(&value["settings"], &value["streamSettings"]),
        _ => bail!("Неподдерживаемый выход Xray"),
    }
}

fn validate_vless(settings: &Value, stream: &Value) -> Result<()> {
    fields(settings, &["vnext"])?;
    let server = single(&settings["vnext"])?;
    fields(server, &["address", "port", "users"])?;
    endpoint(server)?;
    let account = fields(single(&server["users"])?, &["id", "encryption", "flow"])?;
    ensure!(
        uuid::Uuid::parse_str(string(account, "id")?).is_ok(),
        "Некорректный UUID VLESS"
    );
    ensure!(
        string(account, "encryption")? == "none",
        "Поддерживается шифрование VLESS none"
    );
    ensure!(
        matches!(
            string(account, "flow")?,
            "xtls-rprx-vision" | "xtls-rprx-vision-udp443"
        ),
        "Требуется XTLS Vision"
    );
    fields(stream, &["network", "security", "realitySettings"])?;
    ensure!(
        stream["network"] == "raw" && stream["security"] == "reality",
        "Требуется REALITY поверх RAW/TCP"
    );
    let reality = fields(
        &stream["realitySettings"],
        &["serverName", "publicKey", "shortId", "fingerprint"],
    )?;
    host(string(reality, "serverName")?)?;
    ensure!(
        base64_url_bytes(string(reality, "publicKey")?, 32),
        "Некорректный публичный ключ REALITY"
    );
    if let Some(short_id) = optional(reality, "shortId")? {
        ensure!(
            short_id.len() <= 16
                && short_id.len().is_multiple_of(2)
                && short_id.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "Некорректный short-id REALITY"
        );
    }
    if let Some(value) = optional(reality, "fingerprint")? {
        validate_fingerprint(value)?;
    }
    Ok(())
}

fn validate_hysteria(settings: &Value, stream: &Value) -> Result<()> {
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
    ensure!(
        settings["version"] == 2 && stream["network"] == "hysteria" && stream["security"] == "tls",
        "Требуется Hysteria2 с TLS"
    );
    let transport = fields(&stream["hysteriaSettings"], &["version", "auth"])?;
    ensure!(
        transport.get("version").and_then(Value::as_u64) == Some(2),
        "Требуется транспорт Hysteria2"
    );
    string(transport, "auth")?;
    let tls = fields(
        &stream["tlsSettings"],
        &["serverName", "alpn", "pinnedPeerCertSha256"],
    )?;
    let name = string(tls, "serverName")?;
    ensure!(
        !name.eq_ignore_ascii_case("fromMitm"),
        "Неподдерживаемое имя TLS"
    );
    host(name)?;
    if let Some(alpn) = tls.get("alpn") {
        ensure!(
            alpn.as_array().is_some_and(|items| !items.is_empty()
                && items
                    .iter()
                    .all(|item| item.as_str().is_some_and(|value| !value.is_empty()
                        && value.len() <= 255
                        && !value.chars().any(char::is_control)))),
            "Некорректный TLS ALPN"
        );
    }
    if let Some(pin) = optional(tls, "pinnedPeerCertSha256")? {
        let pin = pin.replace(':', "");
        ensure!(
            pin.len() == 64 && pin.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "Некорректный отпечаток TLS-сертификата"
        );
    }
    if let Some(value) = stream.get("finalmask") {
        validate_mask(value)?;
    }
    Ok(())
}

fn validate_mask(value: &Value) -> Result<()> {
    let mask = fields(value, &["udp", "quicParams"])?;
    if let Some(udp) = mask.get("udp") {
        let entry = single(udp)?;
        fields(entry, &["type", "settings"])?;
        ensure!(entry["type"] == "salamander", "Неподдерживаемая UDP-маска");
        string(fields(&entry["settings"], &["password"])?, "password")?;
    }
    if let Some(value) = mask.get("quicParams") {
        let quic = fields(
            value,
            &["congestion", "brutalUp", "brutalDown", "bbrProfile"],
        )?;
        if let Some(congestion) = optional(quic, "congestion")? {
            ensure!(congestion == "brutal", "Неподдерживаемый алгоритм QUIC");
        }
        if let Some(profile) = optional(quic, "bbrProfile")? {
            ensure!(
                matches!(profile, "standard" | "conservative" | "aggressive"),
                "Некорректный профиль BBR"
            );
        }
        for name in ["brutalUp", "brutalDown"] {
            if let Some(speed) = optional(quic, name)? {
                let speed = bandwidth(speed)?;
                ensure!(
                    speed == 0.0 || speed >= 65536.0,
                    "Скорость Brutal должна быть не меньше 65536 байт/с"
                );
            }
        }
    }
    Ok(())
}

fn fields<'a>(value: &'a Value, allowed: &[&str]) -> Result<&'a Map<String, Value>> {
    let options = value.as_object().context("Ожидается объект Xray")?;
    ensure!(
        options.keys().all(|key| allowed.contains(&key.as_str())),
        "Профиль содержит посторонние параметры Xray"
    );
    Ok(options)
}

fn single(value: &Value) -> Result<&Value> {
    match value.as_array().map(Vec::as_slice) {
        Some([item]) => Ok(item),
        _ => bail!("Требуется ровно один элемент конфигурации"),
    }
}

fn string<'a>(fields: &'a Map<String, Value>, key: &'static str) -> Result<&'a str> {
    optional(fields, key)?
        .filter(|text| !text.is_empty())
        .with_context(|| format!("Отсутствует поле {key}"))
}

fn optional<'a>(fields: &'a Map<String, Value>, key: &'static str) -> Result<Option<&'a str>> {
    fields
        .get(key)
        .map(|value| {
            value
                .as_str()
                .with_context(|| format!("Ожидается строка в поле {key}"))
        })
        .transpose()
}

fn endpoint(value: &Value) -> Result<()> {
    host(
        value["address"]
            .as_str()
            .context("Отсутствует адрес узла")?,
    )?;
    ensure!(
        value["port"]
            .as_u64()
            .is_some_and(|port| (1..=65535).contains(&port)),
        "Некорректный порт узла"
    );
    Ok(())
}

fn host(value: &str) -> Result<()> {
    if value.parse::<IpAddr>().is_ok() {
        return Ok(());
    }
    let host = value.strip_suffix('.').unwrap_or(value);
    ensure!(
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
        "Некорректный адрес сервера или SNI, используйте IP либо DNS-имя в ASCII"
    );
    Ok(())
}

fn base64_url_bytes(value: &str, size: usize) -> bool {
    let expected = (size * 8).div_ceil(6);
    if value.len() != expected {
        return false;
    }
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut last = 0;
    for byte in value.bytes() {
        let Some(index) = alphabet.iter().position(|item| *item == byte) else {
            return false;
        };
        last = index;
    }
    let unused_bits = expected * 6 - size * 8;
    last & ((1 << unused_bits) - 1) == 0
}

fn bandwidth(value: &str) -> Result<f64> {
    let value = value.trim().to_ascii_lowercase();
    if value.is_empty() {
        return Ok(0.0);
    }
    let end = value
        .find(|character: char| !character.is_ascii_digit() && character != '.')
        .unwrap_or(value.len());
    let number: f64 = value[..end]
        .parse()
        .map_err(|_| anyhow::anyhow!("Некорректная скорость QUIC"))?;
    let factor = match value[end..].trim() {
        "" | "b" | "bps" => 1.0,
        "k" | "kb" | "kbps" => 1024.0,
        "m" | "mb" | "mbps" => 1024.0 * 1024.0,
        "g" | "gb" | "gbps" => 1024.0 * 1024.0 * 1024.0,
        "t" | "tb" | "tbps" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => bail!("Неизвестная единица скорости QUIC"),
    };
    let speed = number * factor / 8.0;
    ensure!(
        speed.is_finite() && speed >= 0.0 && speed <= u64::MAX as f64,
        "Некорректная скорость QUIC"
    );
    Ok(speed)
}

fn validate_fingerprint(value: &str) -> Result<()> {
    const FINGERPRINTS: &[&str] = &[
        "",
        "chrome",
        "firefox",
        "safari",
        "ios",
        "android",
        "edge",
        "360",
        "qq",
        "random",
        "randomized",
        "randomizednoalpn",
        "hellofirefox_120",
        "hellofirefox_148",
        "hellochrome_120",
        "hellochrome_131",
        "hellochrome_133",
        "helloios_13",
        "helloios_14",
        "helloedge_106",
        "hellosafari_26_3",
        "hello360_11_0",
        "helloqq_11_1",
        "hellorandomized",
        "hellorandomizedalpn",
        "hellorandomizednoalpn",
        "hellofirefox_auto",
        "hellofirefox_55",
        "hellofirefox_56",
        "hellofirefox_63",
        "hellofirefox_65",
        "hellofirefox_99",
        "hellofirefox_102",
        "hellofirefox_105",
        "hellochrome_auto",
        "hellochrome_58",
        "hellochrome_62",
        "hellochrome_70",
        "hellochrome_72",
        "hellochrome_83",
        "hellochrome_87",
        "hellochrome_96",
        "hellochrome_100",
        "hellochrome_102",
        "hellochrome_106_shuffle",
        "helloios_auto",
        "helloios_11_1",
        "helloios_12_1",
        "helloandroid_11_okhttp",
        "helloedge_85",
        "helloedge_auto",
        "hellosafari_16_0",
        "hellosafari_auto",
        "hello360_auto",
        "hello360_7_5",
        "helloqq_auto",
        "hellochrome_100_psk",
        "hellochrome_112_psk_shuf",
        "hellochrome_114_padding_psk_shuf",
        "hellochrome_115_pq",
        "hellochrome_115_pq_psk",
        "hellochrome_120_pq",
    ];
    let value = value.to_ascii_lowercase();
    ensure!(
        FINGERPRINTS.contains(&value.as_str()),
        "Неизвестный или неподдерживаемый fingerprint для Xray 26.7.28"
    );
    Ok(())
}
