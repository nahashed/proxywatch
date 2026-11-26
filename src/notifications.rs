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
    sections: Vec<Vec<Line>>,
}

enum Line {
    Heading(&'static str),
    Field(&'static str, String),
    Text(String),
}

impl Line {
    fn render(&self, html: bool) -> String {
        let escape = |text: &str| {
            if html {
                text.replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;")
            } else {
                text.to_owned()
            }
        };
        let bold = |text: &str| {
            if html {
                format!("<b>{}</b>", escape(text))
            } else {
                text.to_owned()
            }
        };
        match self {
            Self::Heading(label) => bold(label),
            Self::Field(label, value) => {
                format!("{} {}", bold(&format!("{label}:")), escape(value))
            }
            Self::Text(value) => escape(value),
        }
    }
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
        let mut identity = vec![Line::Field(words.instance, title.clone())];
        if !settings.instance_description.trim().is_empty() {
            identity.push(Line::Field(
                words.purpose,
                safe_label(&settings.instance_description, words.unspecified),
            ));
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
        let mut nodes = Vec::new();
        if event.total != 0 || event.started_at != 0 {
            nodes.push(Line::Field(
                words.available,
                format!("{} {} {}", event.available, words.of, event.total),
            ));
        }
        nodes.push(Line::Field(words.route, route));
        if event.active.is_some() && event.active_probe == Some(false) {
            nodes.push(Line::Text(words.route_probe_failed.into()));
        }
        let mut sections = vec![vec![Line::Heading(summary)], identity, nodes];

        let repeats_active = event.kind == "snapshot"
            && event.total == 1
            && event.available == 1
            && matches!(event.changes.as_slice(), [(name, true)] if Some(name) == event.active.as_ref().or(event.to.as_ref()));
        if !event.changes.is_empty() && !repeats_active {
            let heading = if event.kind == "snapshot" {
                words.node_states
            } else {
                words.changed_nodes
            };
            let mut details = vec![Line::Heading(heading)];
            for (name, available) in event.changes.iter().take(5) {
                let state = if *available { words.up } else { words.down };
                details.push(Line::Text(format!(
                    "{}: {state}",
                    safe_label(name, words.unnamed)
                )));
            }
            if event.changes.len() > 5 {
                details.push(Line::Field(
                    words.more,
                    (event.changes.len() - 5).to_string(),
                ));
            }
            sections.push(details);
        }

        let time_block = |label, seconds| {
            let mut block = vec![Line::Heading(label)];
            if let Some((utc, local)) = crate::server_time::event_times(lang, seconds) {
                block.push(Line::Text(utc));
                if let Some(local) = local {
                    block.push(Line::Field(words.server_time, local));
                }
            } else {
                block.push(Line::Text(words.unknown_time.into()));
            }
            block
        };
        sections.push(time_block(words.event, event.at));
        if event.started_at != 0 && event.started_at / 60 != event.at / 60 {
            sections.push(time_block(words.start, event.started_at));
        }
        Ok(Self {
            title,
            summary,
            sections,
        })
    }

    fn render(&self, html: bool) -> String {
        self.sections
            .iter()
            .map(|section| {
                section
                    .iter()
                    .map(|line| line.render(html))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    pub(crate) fn text(&self) -> String {
        self.render(false)
    }

    fn html(&self) -> String {
        self.render(true)
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
    Telegram::validate_token(&config.token)?;
    let response = client
        .post(format!(
            "https://api.telegram.org/bot{}/sendMessage",
            config.token
        ))
        .timeout(timeout)
        .json(&json!({
            "chat_id": config.chat_id,
            "parse_mode": "HTML",
            "text": alert.html(),
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
        .body(alert.text())
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
