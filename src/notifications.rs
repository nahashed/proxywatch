use crate::storage::{Event, Settings, Smtp, Telegram};
use anyhow::{Result, anyhow, ensure};
use lettre::{
    Message, SmtpTransport, Transport, message::header::ContentType,
    transport::smtp::authentication::Credentials,
};
use reqwest::{Proxy, blocking::Client, redirect::Policy};
use serde::Deserialize;
use serde_json::json;
use std::{io::Read, net::SocketAddr, time::Duration};

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
        let title = safe_label(&settings.instance_name, "Proxywatch");
        let summary = if delayed_outage && event.kind == "outage" {
            "Ранее все узлы были недоступны"
        } else {
            event_label(&event.kind).ok_or_else(|| anyhow!("Неизвестный тип уведомления"))?
        };
        let mut lines = vec![summary.to_owned()];
        if !settings.instance_description.is_empty() {
            lines.push(format!(
                "Назначение: {}",
                safe_label(&settings.instance_description, "не указано")
            ));
        }
        for (name, available) in event.changes.iter().take(5) {
            let (indicator, state) = if *available {
                ("🟢", "доступен")
            } else {
                ("🔴", "недоступен")
            };
            lines.push(format!(
                "{indicator} {}: {state}",
                safe_label(name, "узел без названия")
            ));
        }
        if event.changes.len() > 5 {
            lines.push(format!("Ещё узлов: {}", event.changes.len() - 5));
        }
        let route = if event.kind == "outage" {
            "остановлен".into()
        } else {
            match (&event.from, &event.to) {
                (Some(previous), Some(next))
                    if matches!(event.kind.as_str(), "switch" | "failback") && previous != next =>
                {
                    format!(
                        "{} → {}",
                        safe_label(previous, "узел без названия"),
                        safe_label(next, "узел без названия")
                    )
                }
                _ => event.active.as_deref().or(event.to.as_deref()).map_or_else(
                    || "остановлен".into(),
                    |node| safe_label(node, "узел без названия"),
                ),
            }
        };
        lines.push(format!("Трафик: {route}"));
        if event.total != 0 || event.started_at != 0 {
            lines.push(format!(
                "Доступно узлов: {} из {}",
                event.available, event.total
            ));
        }
        lines.push(format!("Событие: {}", timestamp(event.at)));
        if event.started_at != 0 {
            lines.push(format!("Запуск сервиса: {}", timestamp(event.started_at)));
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

pub(crate) fn event_label(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "snapshot" => "Сводка доступности узлов",
        "nodes" => "Доступность узла изменилась",
        "switch" => "Выход переключён",
        "failback" => "Возврат на приоритетный узел",
        "outage" => "Все узлы недоступны",
        "recovery" => "Соединение восстановлено",
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

pub(crate) fn timestamp(seconds: u64) -> String {
    let Some(date) = i64::try_from(seconds)
        .ok()
        .and_then(|value| time::OffsetDateTime::from_unix_timestamp(value).ok())
    else {
        return "неизвестно".into();
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

pub fn deliver(settings: &Settings, event: &Event, http_proxy: Option<SocketAddr>) -> Result<()> {
    deliver_with(settings, event, http_proxy, send_telegram, send_email)
}

fn deliver_with(
    settings: &Settings,
    event: &Event,
    http_proxy: Option<SocketAddr>,
    send_telegram: impl FnOnce(&Telegram, SocketAddr, &Alert, Duration) -> Result<()>,
    send_email: impl FnOnce(&Smtp, &Alert, Duration) -> Result<()>,
) -> Result<()> {
    if settings.telegram.is_none() && settings.smtp.is_none() {
        return Ok(());
    }
    let alert = Alert::from_event(settings, event, http_proxy.is_some())?;
    let alert = &alert;
    let timeout = Duration::from_secs(settings.timeout_seconds);
    let telegram = settings
        .telegram
        .as_ref()
        .zip(http_proxy)
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
    telegram_result.unwrap_or_else(|| {
        Err(anyhow!(
            "Нет доступного выхода для Telegram. Резервная почта не настроена"
        ))
    })
}

fn send_telegram(
    config: &Telegram,
    address: SocketAddr,
    alert: &Alert,
    timeout: Duration,
) -> Result<()> {
    ensure!(
        address.ip().is_loopback(),
        "Для Telegram нужен локальный выход Xray"
    );
    ensure!(
        config.token.split_once(':').is_some_and(|(id, secret)| {
            !id.is_empty()
                && id.bytes().all(|value| value.is_ascii_digit())
                && !secret.is_empty()
                && secret
                    .bytes()
                    .all(|value| value.is_ascii_alphanumeric() || matches!(value, b'_' | b'-'))
        }),
        "Некорректный формат токена Telegram"
    );
    let proxy = Proxy::all(format!("http://{address}"))
        .map_err(|_| anyhow!("Некорректный адрес прокси для Telegram"))?;
    let client = Client::builder()
        .no_proxy()
        .proxy(proxy)
        .redirect(Policy::none())
        .timeout(timeout)
        .build()
        .map_err(|_| anyhow!("Не удалось создать клиент Telegram"))?;
    let response = client
        .post(format!(
            "https://api.telegram.org/bot{}/sendMessage",
            config.token
        ))
        .json(&json!({
            "chat_id": config.chat_id,
            "parse_mode": "MarkdownV2",
            "text": alert.markdown(),
            "link_preview_options": {"is_disabled": true}
        }))
        .send()
        .map_err(|_| anyhow!("Telegram недоступен через выбранный выход"))?;
    ensure!(
        response.status().is_success(),
        "Telegram отклонил уведомление"
    );
    #[derive(Deserialize)]
    struct Receipt {
        ok: bool,
    }
    let receipt: Receipt = serde_json::from_reader(response.take(65536))
        .map_err(|_| anyhow!("Некорректное подтверждение Telegram"))?;
    ensure!(receipt.ok, "Telegram не подтвердил отправку уведомления");
    Ok(())
}

fn send_email(config: &Smtp, alert: &Alert, timeout: Duration) -> Result<()> {
    let email = Message::builder()
        .from(
            config
                .from
                .parse()
                .map_err(|_| anyhow!("Некорректный адрес отправителя SMTP"))?,
        )
        .to(config
            .to
            .parse()
            .map_err(|_| anyhow!("Некорректный адрес получателя SMTP"))?)
        .subject(format!("{}: {}", alert.title, alert.summary))
        .header(ContentType::TEXT_PLAIN)
        .body(format!("{}\n\n{}", alert.title, alert.text))
        .map_err(|_| anyhow!("Не удалось сформировать письмо"))?;
    let builder = if config.port == 465 {
        SmtpTransport::relay(&config.host)
    } else {
        SmtpTransport::starttls_relay(&config.host)
    }
    .map_err(|_| anyhow!("Не удалось настроить TLS для SMTP"))?;
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
        .map_err(|_| anyhow!("SMTP-сервер не подтвердил отправку письма"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn telegram_success_stops_and_failure_or_absent_route_uses_email() {
        for (enable_telegram, success, expected) in [
            (true, true, vec!["telegram"]),
            (true, false, vec!["telegram", "smtp"]),
            (false, false, vec!["smtp"]),
        ] {
            let calls = RefCell::new(Vec::new());
            route(
                enable_telegram.then_some(|| {
                    calls.borrow_mut().push("telegram");
                    ensure!(success, "test");
                    Ok(())
                }),
                Some(|| {
                    calls.borrow_mut().push("smtp");
                    Ok(())
                }),
            )
            .unwrap();
            assert_eq!(*calls.borrow(), expected);
        }
    }

    #[test]
    fn shapes_two_paragraphs_and_escapes_markdown_from_node_names() {
        let event = Event {
            at: 1,
            kind: "switch".into(),
            from: Some("One_[#1]".into()),
            to: Some("https://example.invalid/private".into()),
            active: Some("https://example.invalid/private".into()),
            available: 1,
            total: 2,
            started_at: 1,
            changes: vec![("One_[#1]".into(), false)],
        };
        let settings = Settings {
            instance_name: "Тестовый экземпляр".into(),
            ..Settings::default()
        };
        let alert = Alert::from_event(&settings, &event, false).unwrap();
        let text = alert.markdown();
        assert!(text.starts_with("*Тестовый экземпляр*\n\nВыход переключён\n"));
        assert!(text.contains(r"One\_\[\#1\]"));
        assert!(!text.contains("private"));
        assert!(text.contains("\nДоступно узлов: 1 из 2\n"));
        assert!(text.contains("Запуск сервиса: 01\\.01\\.1970 00:00:01 UTC"));
        assert_eq!(text.matches("\n\n").count(), 1);
        assert_eq!(escape_markdown(r"\*+#|"), r"\\\*\+\#\|");
    }

    #[test]
    fn reserve_recovery_distinguishes_changed_node_from_traffic_route() {
        let settings = Settings {
            instance_name: "instance-a".into(),
            instance_description: "Проверка доступности".into(),
            ..Settings::default()
        };
        let event = Event {
            kind: "nodes".into(),
            active: Some("Основной".into()),
            available: 3,
            total: 4,
            changes: vec![("Резерв".into(), true)],
            ..Event::default()
        };
        let alert = Alert::from_event(&settings, &event, false).unwrap();
        assert!(
            alert
                .text
                .contains("\n🟢 Резерв: доступен\nТрафик: Основной\nДоступно узлов: 3 из 4")
        );
        assert!(alert.text.contains("Назначение: Проверка доступности"));
    }

    #[test]
    fn keeps_snapshot_with_hundreds_of_nodes_within_one_telegram_message() {
        let event = Event {
            kind: "snapshot".into(),
            total: 500,
            available: 500,
            changes: (0..500).map(|_| ("🟢".repeat(100), true)).collect(),
            ..Event::default()
        };
        let alert = Alert::from_event(&Settings::default(), &event, false).unwrap();
        assert!(alert.markdown().encode_utf16().count() <= 4096);
        assert!(alert.text.contains("Ещё узлов: 495"));
    }

    #[test]
    fn delayed_outage_and_recovery_both_reach_telegram_when_the_proxy_returns() {
        let settings = Settings {
            telegram: Some(Telegram {
                token: "synthetic".into(),
                chat_id: "test".into(),
            }),
            ..Settings::default()
        };
        let mut event = Event {
            at: 1,
            kind: "outage".into(),
            from: Some("Primary".into()),
            to: None,
            ..Event::default()
        };
        let proxy = "127.0.0.1:10809".parse().unwrap();
        let sent = RefCell::new(Vec::new());
        let send = |_: &Telegram, address: SocketAddr, alert: &Alert, _: Duration| {
            assert_eq!(address, proxy);
            sent.borrow_mut().push(alert.summary);
            Ok(())
        };
        let no_email =
            |_: &Smtp, _: &Alert, _: Duration| -> Result<()> { panic!("SMTP is disabled") };
        assert!(deliver_with(&settings, &event, None, send, no_email).is_err());
        assert!(sent.borrow().is_empty());
        deliver_with(&settings, &event, Some(proxy), send, no_email).unwrap();
        event.kind = "recovery".into();
        event.to = Some("Reserve".into());
        deliver_with(&settings, &event, Some(proxy), send, no_email).unwrap();
        assert_eq!(
            *sent.borrow(),
            ["Ранее все узлы были недоступны", "Соединение восстановлено"]
        );
    }
}
