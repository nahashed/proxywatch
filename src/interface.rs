use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Result, ensure};
use console::{Term, style, truncate_str};
use dialoguer::{Input, MultiSelect, Password, Select, theme::ColorfulTheme};
use reqwest::Url;

use crate::{
    notifications::{Alert, event_label, timestamp},
    storage::{PRESETS, Paths, Settings, Smtp, State, Telegram},
};

pub fn run(paths: &Paths) -> Result<()> {
    let terminal = Term::stderr();
    ensure!(terminal.is_term(), "Для меню нужен интерактивный терминал");
    let ui = Interface {
        terminal,
        theme: ColorfulTheme::default(),
    };
    if !paths.has_settings() && !ui.setup(paths)? {
        return Ok(());
    }
    loop {
        ui.screen("Proxywatch")?;
        let settings = paths.load_settings()?;
        let state = paths.load_state()?;
        ui.status(&settings, &state)?;
        match ui.select(
            "Действие",
            &[
                "Обновить",
                "Узлы",
                "История событий",
                "Настройки",
                "Закрыть меню",
            ],
            0,
        )? {
            Some(0) => (),
            Some(1) => ui.nodes(&state)?,
            Some(2) => ui.history(&settings, &state)?,
            Some(3) => {
                ui.edit(paths, settings)?;
            }
            _ => return Ok(()),
        }
    }
}

struct Interface {
    terminal: Term,
    theme: ColorfulTheme,
}

impl Interface {
    fn line(&self, text: &str) -> Result<()> {
        Ok(self.terminal.write_line(text)?)
    }

    fn screen(&self, title: &str) -> Result<()> {
        self.terminal.clear_screen()?;
        self.line(&format!("{}\n", style(title).bold()))
    }

    fn select<T: std::fmt::Display>(
        &self,
        title: &str,
        choices: &[T],
        default: usize,
    ) -> Result<Option<usize>> {
        let (height, width) = self.terminal.size();
        let rows: Vec<_> = choices
            .iter()
            .map(|item| menu_line(&item.to_string(), width))
            .collect();
        Ok(Select::with_theme(&self.theme)
            .with_prompt(title)
            .items(&rows)
            .max_length(usize::from(height.saturating_sub(5).clamp(1, 12)))
            .default(default)
            .interact_on_opt(&self.terminal)?)
    }

    fn confirm(&self, title: &str, default: bool) -> Result<bool> {
        Ok(self.select(title, &["Да", "Нет"], usize::from(!default))? == Some(0))
    }

    fn text(&self, title: &str, current: &str, optional: bool) -> Result<String> {
        let mut prompt = Input::<String>::with_theme(&self.theme)
            .with_prompt(title)
            .allow_empty(optional);
        if !current.is_empty() {
            prompt = prompt.with_initial_text(current);
        }
        Ok(prompt.interact_text_on(&self.terminal)?.trim().to_owned())
    }

    fn secret(&self, title: &str, current: &str) -> Result<String> {
        let prompt = if current.is_empty() {
            title.to_owned()
        } else {
            format!("{title} (Enter сохраняет прежнее значение)")
        };
        let input = Password::with_theme(&self.theme)
            .with_prompt(prompt)
            .allow_empty_password(!current.is_empty())
            .interact_on(&self.terminal)?;
        if input.is_empty() {
            Ok(current.to_owned())
        } else {
            Ok(input)
        }
    }

    fn positive(&self, title: &str, current: u64, maximum: u64) -> Result<u64> {
        Ok(Input::<u64>::with_theme(&self.theme)
            .with_prompt(title)
            .default(current)
            .validate_with(|value: &u64| {
                (1..=maximum)
                    .contains(value)
                    .then_some(())
                    .ok_or_else(|| format!("Введите число от 1 до {maximum}"))
            })
            .interact_text_on(&self.terminal)?)
    }

    fn pause(&self) -> Result<()> {
        self.line("\nНажмите Enter, чтобы вернуться в меню.")?;
        self.terminal.read_line()?;
        Ok(())
    }

    fn setup(&self, paths: &Paths) -> Result<bool> {
        self.screen("Proxywatch · Первая настройка")?;
        let mut settings = Settings {
            subscription_url: self.secret("Ссылка на подписку Mihomo YAML", "")?,
            ..Settings::default()
        };
        self.identity(&mut settings)?;
        if !self.ports(&mut settings)? || !self.checks(&mut settings)? {
            return Ok(false);
        }
        self.line("Telegram доставляет события через рабочий выход. Для письма при потере всех узлов настройте SMTP.")?;
        self.notifications(&mut settings)?;
        self.edit(paths, settings)
    }

    fn edit(&self, paths: &Paths, mut settings: Settings) -> Result<bool> {
        loop {
            self.screen("Proxywatch · Настройки")?;
            self.line(&display_text(&settings.instance_name))?;
            self.line(&ports_label(&settings))?;
            self.line(&format!(
                "Проверка {} · каждые {} с · порог {}",
                target_label(&settings.check_url),
                settings.interval_seconds,
                settings.failure_threshold
            ))?;
            self.line(&format!(
                "Telegram {} · SMTP {}\n",
                enabled(settings.telegram.is_some()),
                enabled(settings.smtp.is_some())
            ))?;
            match self.select(
                "Настройки",
                &[
                    "Сохранить",
                    "Подписка",
                    "Локальные порты",
                    "Проверки и интервалы",
                    "Уведомления",
                    "Путь к Xray",
                    "Имя и назначение установки",
                    "Выйти без сохранения",
                ],
                0,
            )? {
                Some(0) => {
                    if let Err(error) = settings.validate() {
                        self.line(&format!("Проверьте настройки. {error}"))?;
                        self.pause()?;
                        continue;
                    }
                    let existing = paths.has_settings();
                    paths.save_settings(&settings)?;
                    self.line(if existing {
                        "Настройки сохранены. Чтобы применить их к работающему сервису, перезапустите его. В systemd используйте systemctl --user restart proxywatch."
                    } else {
                        "Настройки сохранены. Запустите сервис командой proxywatch run или используйте systemctl --user enable --now proxywatch после установки unit-файла."
                    })?;
                    self.pause()?;
                    return Ok(true);
                }
                Some(1) => {
                    settings.subscription_url =
                        self.secret("Ссылка на подписку Mihomo YAML", &settings.subscription_url)?
                }
                Some(2) => {
                    self.ports(&mut settings)?;
                }
                Some(3) => {
                    self.checks(&mut settings)?;
                }
                Some(4) => self.notifications(&mut settings)?,
                Some(5) => {
                    settings.xray_binary = self.text(
                        "Путь или имя исполняемого файла Xray",
                        &settings.xray_binary,
                        false,
                    )?
                }
                Some(6) => self.identity(&mut settings)?,
                _ => return Ok(false),
            }
        }
    }

    fn identity(&self, settings: &mut Settings) -> Result<()> {
        settings.instance_name = self.text("Имя установки", &settings.instance_name, false)?;
        settings.instance_description = self.text(
            "Назначение (необязательно)",
            &settings.instance_description,
            true,
        )?;
        Ok(())
    }

    fn ports(&self, settings: &mut Settings) -> Result<bool> {
        let default = match (settings.socks_port, settings.http_port) {
            (Some(_), None) => 1,
            (None, Some(_)) => 2,
            _ => 0,
        };
        let Some(mode) = self.select(
            "Локальные входы на 127.0.0.1",
            &[
                "SOCKS5 и HTTP на отдельных портах",
                "Только SOCKS5",
                "Только HTTP",
            ],
            default,
        )?
        else {
            return Ok(false);
        };
        let socks = if mode != 2 {
            Some(self.positive(
                "Порт SOCKS5",
                u64::from(settings.socks_port.unwrap_or(10808)),
                u16::MAX.into(),
            )? as u16)
        } else {
            None
        };
        let http = if mode != 1 {
            loop {
                let port = self.positive(
                    "Порт HTTP",
                    u64::from(settings.http_port.unwrap_or(10809)),
                    u16::MAX.into(),
                )? as u16;
                if Some(port) != socks {
                    break Some(port);
                }
                self.line("Этот порт уже выбран для SOCKS5. Выберите другой порт для HTTP.")?;
            }
        } else {
            None
        };
        settings.socks_port = socks;
        settings.http_port = http;
        Ok(true)
    }

    fn checks(&self, settings: &mut Settings) -> Result<bool> {
        let mut choices: Vec<_> = PRESETS.iter().map(|(label, _)| *label).collect();
        choices.push("Свой URL");
        let default = PRESETS
            .iter()
            .position(|(_, url)| *url == settings.check_url)
            .unwrap_or(PRESETS.len());
        let Some(index) = self.select(
            "От доступности какого сервиса зависит переключение?",
            &choices,
            default,
        )?
        else {
            return Ok(false);
        };
        let selected = if index < PRESETS.len() {
            PRESETS[index].1.to_owned()
        } else {
            self.secret("URL контрольного запроса", &settings.check_url)?
        };
        let period = self.positive(
            "Проверка активного узла, с",
            settings.interval_seconds,
            86400,
        )?;
        let reserve_period = self.positive(
            "Проверка резервных узлов, с",
            settings.reserve_interval_seconds,
            86400,
        )?;
        let threshold = self.positive(
            "Ошибок подряд до переключения",
            settings.failure_threshold.into(),
            100,
        )? as u32;
        let timeout = self.positive("Тайм-аут запроса, с", settings.timeout_seconds, 300)?;
        let refresh = self.positive(
            "Интервал обновления подписки, с",
            settings.refresh_seconds,
            2_592_000,
        )?;
        let mut diagnostics = settings.diagnostic_urls.clone();
        diagnostics.retain(|url| url != &selected);
        if self.confirm("Выбрать дополнительные диагностические проверки?", false)?
        {
            let mut options: Vec<String> = PRESETS
                .iter()
                .map(|(_, url)| (*url).to_owned())
                .filter(|url| url != &selected)
                .collect();
            for url in &diagnostics {
                if !options.contains(url) {
                    options.push(url.clone())
                }
            }
            let names: Vec<_> = options.iter().map(|url| target_label(url)).collect();
            let defaults: Vec<_> = options
                .iter()
                .map(|url| diagnostics.contains(url))
                .collect();
            self.line(
                "Пробел отмечает сервис, Enter сохраняет выбор. Эти результаты видны в статусе.",
            )?;
            let Some(indices) = MultiSelect::with_theme(&self.theme)
                .with_prompt("Диагностические проверки")
                .items(&names)
                .defaults(&defaults)
                .interact_on_opt(&self.terminal)?
            else {
                return Ok(false);
            };
            diagnostics = indices
                .into_iter()
                .map(|index| options[index].clone())
                .collect();
            while diagnostics.len() < 16
                && self.confirm("Добавить свой диагностический URL?", false)?
            {
                let url = self.secret("Диагностический URL", "")?;
                if url != selected && !diagnostics.contains(&url) {
                    diagnostics.push(url)
                }
            }
        }
        settings.check_url = selected;
        settings.interval_seconds = period;
        settings.reserve_interval_seconds = reserve_period;
        settings.failure_threshold = threshold;
        settings.timeout_seconds = timeout;
        settings.refresh_seconds = refresh;
        settings.diagnostic_urls = diagnostics;
        Ok(true)
    }

    fn notifications(&self, settings: &mut Settings) -> Result<()> {
        let choices = ["Telegram через активный узел", "Почта через SMTP напрямую"];
        self.line("Пробел отмечает канал, Enter подтверждает выбор.")?;
        let Some(selected) = MultiSelect::with_theme(&self.theme)
            .with_prompt("Каналы уведомлений")
            .items(choices)
            .defaults(&[settings.telegram.is_some(), settings.smtp.is_some()])
            .interact_on_opt(&self.terminal)?
        else {
            return Ok(());
        };
        let telegram = if selected.contains(&0) {
            let existing = settings.telegram.as_ref();
            Some(Telegram {
                token: self.secret(
                    "Токен бота Telegram",
                    existing.map_or("", |t| t.token.as_str()),
                )?,
                chat_id: self.text(
                    "ID чата Telegram",
                    existing.map_or("", |t| t.chat_id.as_str()),
                    false,
                )?,
            })
        } else {
            None
        };
        let smtp = if selected.contains(&1) {
            let existing = settings.smtp.as_ref();
            let host = self.text(
                "Сервер SMTP",
                existing.map_or("", |s| s.host.as_str()),
                false,
            )?;
            let port = self.positive(
                "Порт SMTP",
                existing.map_or(465, |s| u64::from(s.port)),
                u16::MAX.into(),
            )? as u16;
            let username = self.text(
                "Имя пользователя SMTP (можно оставить пустым)",
                existing.map_or("", |s| s.username.as_str()),
                true,
            )?;
            let password = if username.is_empty() {
                String::new()
            } else {
                self.secret("Пароль SMTP", existing.map_or("", |s| s.password.as_str()))?
            };
            Some(Smtp {
                host,
                port,
                username,
                password,
                from: self.text(
                    "Адрес отправителя",
                    existing.map_or("", |s| s.from.as_str()),
                    false,
                )?,
                to: self.text(
                    "Адрес получателя",
                    existing.map_or("", |s| s.to.as_str()),
                    false,
                )?,
            })
        } else {
            None
        };
        settings.telegram = telegram;
        settings.smtp = smtp;
        Ok(())
    }

    fn status(&self, settings: &Settings, state: &State) -> Result<()> {
        self.line(&display_text(&settings.instance_name))?;
        self.line(&ports_label(settings))?;
        if state.updated_at == 0 {
            self.line("Сервис ещё не записал состояние.")?;
            return Ok(());
        }
        let status = match state.status.as_str() {
            "starting" => "запускается",
            "healthy" => "работает",
            "degraded" => "ошибки проверки",
            "outage" => "все узлы недоступны",
            "stopped" => "остановлен",
            _ => "неизвестно",
        };
        self.line(&format!(
            "Состояние · {} · {}",
            status,
            elapsed_label(now(), state.updated_at)
        ))?;
        self.line(&format!(
            "Доступно узлов: {} из {}",
            state
                .nodes
                .values()
                .filter(|health| health.available == Some(true))
                .count(),
            state.nodes.len()
        ))?;
        if let Some(preferred) = state.preferred.as_deref() {
            self.line(&format!("Приоритет: {}", display_text(preferred)))?;
        }
        self.line(&format!(
            "Активный узел · {}",
            state
                .active
                .as_deref()
                .map(display_text)
                .unwrap_or_else(|| "отсутствует".into())
        ))?;
        self.line(&format!(
            "Проверка · {} · {}",
            target_label(&settings.check_url),
            state.last_probe.map_or("ожидается", probe_label)
        ))?;
        for (target, passed) in &state.diagnostics {
            self.line(&format!(
                "Диагностика · {} · {}",
                target_label(target),
                probe_label(*passed)
            ))?;
        }
        for reason in &state.skipped {
            self.line(&format!("Пропущено · {}", display_text(reason)))?;
        }
        Ok(())
    }

    fn nodes(&self, state: &State) -> Result<()> {
        if state.nodes.is_empty() {
            self.screen("Узлы")?;
            self.line("Результатов проверки пока нет.")?;
            return self.pause();
        }
        let entries: Vec<_> = state.nodes.iter().collect();
        let mut choices: Vec<_> = entries
            .iter()
            .map(|(name, health)| {
                let indicator = match health.available {
                    Some(true) => "+",
                    Some(false) => "−",
                    None => "?",
                };
                format!("{indicator} {}", display_text(name))
            })
            .collect();
        choices.push("Назад".into());
        let mut selected = 0;
        loop {
            self.screen("Узлы")?;
            self.line("+ доступен  − недоступен  ? не проверен")?;
            let Some(index) = self.select("Выберите узел", &choices, selected)? else {
                return Ok(());
            };
            if index == entries.len() {
                return Ok(());
            }
            selected = index;
            let (name, health) = entries[index];
            self.screen(&display_text(name))?;
            let status = match health.available {
                Some(true) => "доступен",
                Some(false) => "недоступен",
                None => "ожидает проверки",
            };
            self.line(&format!("Состояние: {status}"))?;
            if state.active.as_ref() == Some(name) {
                self.line("Сейчас передаёт трафик")?;
            }
            if state.preferred.as_ref() == Some(name) {
                self.line("Приоритетный выход")?;
            }
            if health.since != 0 {
                self.line(&format!("Состояние с {}", timestamp(health.since)))?;
            }
            if health.checked_at != 0 {
                self.line(&format!("Проверка: {}", timestamp(health.checked_at)))?;
            }
            if let Some(latency) = health.latency_ms {
                self.line(&format!("Отклик: {latency} мс"))?;
            }
            self.pause()?;
        }
    }

    fn history(&self, settings: &Settings, state: &State) -> Result<()> {
        self.screen("История событий")?;
        if state.history.is_empty() {
            self.line("Событий пока нет.")?;
        } else {
            let events: Vec<_> = state.history.iter().rev().collect();
            let time = now();
            let mut labels: Vec<_> = events
                .iter()
                .map(|event| {
                    format!(
                        "{} · {}",
                        event_label(&event.kind).unwrap_or("Событие"),
                        elapsed_label(time, event.at)
                    )
                })
                .collect();
            labels.push("Назад".into());
            let mut selected = 0;
            loop {
                self.screen("История событий")?;
                let Some(index) = self.select("Выберите событие", &labels, selected)?
                else {
                    return Ok(());
                };
                if index == events.len() {
                    return Ok(());
                }
                selected = index;
                let event = events[index];
                self.screen("Подробности события")?;
                let alert = Alert::from_event(settings, event, false)?;
                self.line(&alert.text)?;
                for (name, available) in event.changes.iter().skip(5) {
                    self.line(&format!(
                        "{}: {}",
                        display_text(name),
                        if *available {
                            "доступен"
                        } else {
                            "недоступен"
                        }
                    ))?;
                }
                self.pause()?;
            }
        }
        self.pause()
    }
}

fn menu_line(text: &str, width: u16) -> String {
    truncate_str(text, usize::from(width.saturating_sub(4)).max(1), "…").into_owned()
}

fn ports_label(settings: &Settings) -> String {
    let mut endpoints = Vec::new();
    if let Some(port) = settings.socks_port {
        endpoints.push(format!("SOCKS5 127.0.0.1:{port}"));
    }
    if let Some(port) = settings.http_port {
        endpoints.push(format!("HTTP 127.0.0.1:{port}"));
    }
    endpoints.join("\n")
}

fn display_text(text: &str) -> String {
    text.chars()
        .filter(|c| {
            !c.is_control() && !matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
        .take(100)
        .collect()
}

fn target_label(url: &str) -> String {
    if let Some((name, _)) = PRESETS.iter().find(|(_, address)| *address == url) {
        return (*name).to_owned();
    }
    Url::parse(url)
        .ok()
        .and_then(|address| address.host_str().map(display_text))
        .unwrap_or_else(|| "Свой URL".to_owned())
}

fn enabled(configured: bool) -> &'static str {
    if configured {
        "включён"
    } else {
        "выключен"
    }
}

fn probe_label(passed: bool) -> &'static str {
    if passed {
        "доступен"
    } else {
        "ошибка"
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn elapsed_label(now: u64, timestamp: u64) -> String {
    match now.saturating_sub(timestamp) {
        0..=4 => "только что".to_owned(),
        seconds @ 5..=59 => format!("{seconds} с назад"),
        seconds @ 60..=3599 => format!("{} мин назад", seconds / 60),
        seconds @ 3600..=86399 => format!("{} ч назад", seconds / 3600),
        seconds => format!("{} дн назад", seconds / 86400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_target_label_hides_path_credentials_and_query() {
        assert_eq!(
            target_label("https://user:password@example.org/private/token?key=secret"),
            "example.org"
        );
        assert_eq!(target_label("private-token"), "Свой URL");
    }

    #[test]
    fn untrusted_names_cannot_control_the_terminal() {
        assert_eq!(display_text("A\r\n\u{1b}\u{202e}B"), "AB");
    }

    #[test]
    fn phone_menu_rows_fit_without_losing_full_detail_text() {
        let label = "🟢 Тестовый узел с длинным именем для узкого экрана";
        let compact = menu_line(label, 28);
        assert!(console::measure_text_width(&compact) <= 24);
        assert!(compact.ends_with('…'));
        assert_eq!(menu_line(label, 100), label);
    }
}
