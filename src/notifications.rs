use crate::alert_text::alerts;
use crate::i18n::Language;
use crate::i18n::language;
use crate::storage::{Event, Settings, Smtp, Telegram};
use anyhow::{Result, anyhow, ensure};
use lettre::{
    Message, SmtpTransport, Transport, message::header::ContentType,
    transport::smtp::authentication::Credentials,
};
use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::json;
use std::{io::Read, time::Duration};

pub(crate) struct Alert {
    pub(crate) title: String,
    pub(crate) summary: &'static str,
    pub(crate) text: String,
}

impl Alert {
    pub(crate) fn from_event(
        settings: &Settings,
        event: &Event,
        delayed_outage: bool,
    ) -> Result<Self> {
        let lang = settings.language;
        let words = alerts(lang);
        let title = safe_label(&settings.instance_name, "Proxywatch");
        let summary = if delayed_outage && event.kind == "outage" {
            words.delayed
        } else {
            event_label(lang, &event.kind).ok_or_else(|| anyhow!(words.unknown_kind))?
        };
        let mut lines = vec![summary.to_owned()];
        if !settings.instance_description.is_empty() {
            lines.push(format!(
                "{}: {}",
                words.purpose,
                safe_label(&settings.instance_description, words.unspecified)
            ));
        }
        for (name, available) in event.changes.iter().take(5) {
            let (indicator, state) = if *available {
                ("🟢", words.up)
            } else {
                ("🔴", words.down)
            };
            lines.push(format!(
                "{indicator} {}: {state}",
                safe_label(name, words.unnamed)
            ));
        }
        if event.changes.len() > 5 {
            lines.push(format!("{}: {}", words.more, event.changes.len() - 5));
        }
        let route = if event.kind == "outage" {
            words.stopped.into()
        } else {
            match (&event.from, &event.to) {
                (Some(previous), Some(next))
                    if matches!(event.kind.as_str(), "switch" | "failback") && previous != next =>
                {
                    format!(
                        "{} → {}",
                        safe_label(previous, words.unnamed),
                        safe_label(next, words.unnamed)
                    )
                }
                _ => event.active.as_deref().or(event.to.as_deref()).map_or_else(
                    || words.stopped.into(),
                    |node| safe_label(node, words.unnamed),
                ),
            }
        };
        lines.push(format!("{}: {route}", words.route));
        if event.total != 0 || event.started_at != 0 {
            lines.push(format!(
                "{}: {} {} {}",
                words.available, event.available, words.of, event.total
            ));
        }
        lines.push(format!("{}: {}", words.event, timestamp(lang, event.at)));
        if event.started_at != 0 {
            lines.push(format!(
                "{}: {}",
                words.start,
                timestamp(lang, event.started_at)
            ));
        }
        Ok(Self {
            title,
            summary,
            text: lines.join("\n"),
        })
    }

    fn markdown(&self) -> String {
        format!(
            "*{}*\n\n{}",
            escape_markdown(&self.title),
            escape_markdown(&self.text)
        )
    }
}

pub(crate) fn event_label(lang: Language, kind: &str) -> Option<&'static str> {
    let words = alerts(lang);
    Some(match kind {
        "snapshot" => words.snapshot,
        "nodes" => words.nodes,
        "switch" => words.switch,
        "failback" => words.failback,
        "outage" => words.outage,
        "recovery" => words.recovery,
        _ => return None,
    })
}

fn safe_label(value: &str, fallback: &str) -> String {
    let value = value.trim();
    if value.is_empty()
        || value.contains("://")
        || value.chars().any(|character| {
            character.is_control()
                || matches!(character, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
    {
        return fallback.into();
    }
    value.chars().take(100).collect()
}

pub(crate) fn timestamp(lang: Language, seconds: u64) -> String {
    let Some(date) = i64::try_from(seconds)
        .ok()
        .and_then(|value| time::OffsetDateTime::from_unix_timestamp(value).ok())
    else {
        return alerts(lang).unknown_time.into();
    };
    format!(
        "{:02}.{:02}.{:04} {:02}:{:02}:{:02} UTC",
        date.day(),
        date.month() as u8,
        date.year(),
        date.hour(),
        date.minute(),
        date.second()
    )
}

fn escape_markdown(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        if r"\_*[]()~`>#+-=|{}.!".contains(ch) {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

pub fn deliver(settings: &Settings, event: &Event, proxy_client: Option<&Client>) -> Result<()> {
    deliver_with(settings, event, proxy_client, send_telegram, send_email)
}

fn deliver_with(
    settings: &Settings,
    event: &Event,
    proxy_client: Option<&Client>,
    send_telegram: impl FnOnce(&Telegram, &Client, &Alert, Duration) -> Result<()>,
    send_email: impl FnOnce(&Smtp, &Alert, Duration) -> Result<()>,
) -> Result<()> {
    if settings.telegram.is_none() && settings.smtp.is_none() {
        return Ok(());
    }
    let alert = Alert::from_event(settings, event, proxy_client.is_some())?;
    let alert = &alert;
    let timeout = Duration::from_secs(settings.timeout_seconds);
    let telegram = settings
        .telegram
        .as_ref()
        .zip(proxy_client)
        .filter(|_| event.kind != "outage" || settings.smtp.is_none());
    route(
        telegram.map(|(config, address)| move || send_telegram(config, address, alert, timeout)),
        settings
            .smtp
            .as_ref()
            .map(|config| move || send_email(config, alert, timeout)),
    )
}

fn route(
    telegram: Option<impl FnOnce() -> Result<()>>,
    smtp: Option<impl FnOnce() -> Result<()>>,
) -> Result<()> {
    let telegram_result = telegram.map(|send| send());
    if matches!(telegram_result, Some(Ok(()))) {
        return Ok(());
    }
    if let Some(send) = smtp {
        return send();
    }
    telegram_result.unwrap_or_else(|| Err(anyhow!(alerts(language()).no_delivery)))
}

fn send_telegram(
    config: &Telegram,
    client: &Client,
    alert: &Alert,
    timeout: Duration,
) -> Result<()> {
    ensure!(
        config.token.split_once(':').is_some_and(|(id, secret)| {
            !id.is_empty()
                && id.bytes().all(|value| value.is_ascii_digit())
                && !secret.is_empty()
                && secret
                    .bytes()
                    .all(|value| value.is_ascii_alphanumeric() || matches!(value, b'_' | b'-'))
        }),
        alerts(language()).token
    );
    let response = client
        .post(format!(
            "https://api.telegram.org/bot{}/sendMessage",
            config.token
        ))
        .timeout(timeout)
        .json(&json!({
            "chat_id": config.chat_id,
            "parse_mode": "MarkdownV2",
            "text": alert.markdown(),
            "link_preview_options": {"is_disabled": true}
        }))
        .send()
        .map_err(|_| anyhow!(alerts(language()).unreachable))?;
    ensure!(response.status().is_success(), alerts(language()).rejected);
    #[derive(Deserialize)]
    struct Receipt {
        ok: bool,
    }
    let receipt: Receipt = serde_json::from_reader(response.take(65536))
        .map_err(|_| anyhow!(alerts(language()).receipt))?;
    ensure!(receipt.ok, alerts(language()).unconfirmed);
    Ok(())
}

fn send_email(config: &Smtp, alert: &Alert, timeout: Duration) -> Result<()> {
    let email = Message::builder()
        .from(
            config
                .from
                .parse()
                .map_err(|_| anyhow!(alerts(language()).sender))?,
        )
        .to(config
            .to
            .parse()
            .map_err(|_| anyhow!(alerts(language()).recipient))?)
        .subject(format!("{}: {}", alert.title, alert.summary))
        .header(ContentType::TEXT_PLAIN)
        .body(format!("{}\n\n{}", alert.title, alert.text))
        .map_err(|_| anyhow!(alerts(language()).compose))?;
    let builder = if config.port == 465 {
        SmtpTransport::relay(&config.host)
    } else {
        SmtpTransport::starttls_relay(&config.host)
    }
    .map_err(|_| anyhow!(alerts(language()).tls))?;
    let builder = if config.username.is_empty() && config.password.is_empty() {
        builder
    } else {
        builder.credentials(Credentials::new(
            config.username.clone(),
            config.password.clone(),
        ))
    };
    let transport = builder.port(config.port).timeout(Some(timeout)).build();
    transport
        .send(&email)
        .map_err(|_| anyhow!(alerts(language()).delivery))?;
    Ok(())
}

#[cfg(test)]
#[path = "../tests/unit/notifications.rs"]
mod tests;
