use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Result, ensure};
use console::{Term, style, truncate_str};
use dialoguer::{Input, MultiSelect, Password, Select, theme::ColorfulTheme};
use reqwest::Url;

use crate::{
    i18n::Language,
    notifications::{Alert, event_label, timestamp},
    storage::{PRESETS, Paths, Settings, Smtp, State, Telegram},
};

include!("../locales/en/tui.rs");
include!("../locales/ru/tui.rs");

macro_rules! tui {
    ($language:expr, $message:ident) => {
        match $language {
            Language::En => tui_en!($message),
            Language::Ru => tui_ru!($message),
        }
    };
    ($language:expr, $message:ident, $($args:expr),+ $(,)?) => {
        match $language {
            Language::En => format!(tui_en!($message), $($args),+),
            Language::Ru => format!(tui_ru!($message), $($args),+),
        }
    };
}

pub fn run(paths: &Paths) -> Result<()> {
    let terminal = Term::stderr();
    let mut ui = Interface {
        language: if paths.has_settings() {
            paths.load_settings()?.language
        } else {
            Language::default()
        },
        terminal,
        theme: ColorfulTheme::default(),
    };
    ensure!(ui.terminal.is_term(), tui!(ui.language, terminal_required));
    if !paths.has_settings() {
        let Some(language) = ui.choose_language()? else {
            return Ok(());
        };
        ui.set_language(language);
        if !ui.setup(paths)? {
            return Ok(());
        }
    }
    loop {
        let settings = paths.load_settings()?;
        ui.set_language(settings.language);
        ui.screen("Proxywatch")?;
        let state = paths.load_state()?;
        ui.status(&settings, &state)?;
        match ui.select(
            tui!(ui.language, action),
            &[
                tui!(ui.language, refresh),
                tui!(ui.language, nodes),
                tui!(ui.language, history),
                tui!(ui.language, settings),
                tui!(ui.language, close),
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
    language: Language,
    terminal: Term,
    theme: ColorfulTheme,
}

impl Interface {
    fn set_language(&mut self, language: Language) {
        self.language = language;
        crate::i18n::set_language(language);
    }

    fn choose_language(&self) -> Result<Option<Language>> {
        Ok(self
            .select(
                tui!(self.language, language_choice),
                &[tui!(self.language, english), tui!(self.language, russian)],
                usize::from(self.language == Language::Ru),
            )?
            .map(|index| {
                if index == 0 {
                    Language::En
                } else {
                    Language::Ru
                }
            }))
    }

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
        Ok(self.select(
            title,
            &[tui!(self.language, yes), tui!(self.language, no)],
            usize::from(!default),
        )? == Some(0))
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
            tui!(self.language, keep_secret, title)
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
        let error = || tui!(self.language, number_range, maximum);
        let input = Input::<String>::with_theme(&self.theme)
            .with_prompt(title)
            .default(current.to_string())
            .validate_with(|text: &String| {
                text.parse::<u64>()
                    .ok()
                    .filter(|value| (1..=maximum).contains(value))
                    .map(|_| ())
                    .ok_or_else(error)
            })
            .interact_text_on(&self.terminal)?;
        input.parse().map_err(|_| anyhow::anyhow!(error()))
    }

    fn pause(&self) -> Result<()> {
        self.line(tui!(self.language, pause))?;
        self.terminal.read_line()?;
        Ok(())
    }

    fn setup(&mut self, paths: &Paths) -> Result<bool> {
        self.screen(tui!(self.language, first_setup))?;
        let mut settings = Settings {
            language: self.language,
            subscription_url: self.secret(tui!(self.language, subscription_url), "")?,
            ..Settings::default()
        };
        self.identity(&mut settings)?;
        if !self.ports(&mut settings)? || !self.checks(&mut settings)? {
            return Ok(false);
        }
        self.line(tui!(self.language, notification_intro))?;
        self.notifications(&mut settings)?;
        self.edit(paths, settings)
    }

    fn edit(&mut self, paths: &Paths, mut settings: Settings) -> Result<bool> {
        loop {
            self.screen(tui!(self.language, settings_title))?;
            self.line(&display_text(&settings.instance_name))?;
            self.line(&ports_label(&settings))?;
            self.line(&tui!(
                self.language,
                check_summary,
                target_label(self.language, &settings.check_url),
                settings.interval_seconds,
                settings.failure_threshold
            ))?;
            self.line(&format!(
                "Telegram {} · SMTP {}\n",
                enabled(self.language, settings.telegram.is_some()),
                enabled(self.language, settings.smtp.is_some())
            ))?;
            match self.select(
                tui!(self.language, settings),
                &[
                    tui!(self.language, save),
                    tui!(self.language, subscription),
                    tui!(self.language, local_ports),
                    tui!(self.language, checks),
                    tui!(self.language, notifications),
                    tui!(self.language, xray_path),
                    tui!(self.language, identity),
                    tui!(self.language, language_choice),
                    tui!(self.language, discard),
                ],
                0,
            )? {
                Some(0) => {
                    if let Err(error) = settings.validate() {
                        self.line(&tui!(self.language, invalid_settings, error))?;
                        self.pause()?;
                        continue;
                    }
                    let existing = paths.has_settings();
                    paths.save_settings(&settings)?;
                    self.line(if existing {
                        tui!(self.language, saved_existing)
                    } else {
                        tui!(self.language, saved_new)
                    })?;
                    self.pause()?;
                    return Ok(true);
                }
                Some(1) => {
                    settings.subscription_url = self.secret(
                        tui!(self.language, subscription_url),
                        &settings.subscription_url,
                    )?
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
                        tui!(self.language, xray_binary),
                        &settings.xray_binary,
                        false,
                    )?
                }
                Some(6) => self.identity(&mut settings)?,
                Some(7) => {
                    if let Some(language) = self.choose_language()? {
                        self.set_language(language);
                        settings.language = language;
                    }
                }
                _ => return Ok(false),
            }
        }
    }

    fn identity(&self, settings: &mut Settings) -> Result<()> {
        settings.instance_name = self.text(
            tui!(self.language, instance_name),
            &settings.instance_name,
            false,
        )?;
        settings.instance_description = self.text(
            tui!(self.language, instance_description),
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
            tui!(self.language, inputs),
            &[
                tui!(self.language, both_inputs),
                tui!(self.language, socks_only),
                tui!(self.language, http_only),
            ],
            default,
        )?
        else {
            return Ok(false);
        };
        let socks = if mode != 2 {
            Some(self.positive(
                tui!(self.language, socks_port),
                u64::from(settings.socks_port.unwrap_or(10808)),
                u16::MAX.into(),
            )? as u16)
        } else {
            None
        };
        let http = if mode != 1 {
            loop {
                let port = self.positive(
                    tui!(self.language, http_port),
                    u64::from(settings.http_port.unwrap_or(10809)),
                    u16::MAX.into(),
                )? as u16;
                if Some(port) != socks {
                    break Some(port);
                }
                self.line(tui!(self.language, port_conflict))?;
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
        choices.push(tui!(self.language, custom_url));
        let default = PRESETS
            .iter()
            .position(|(_, url)| *url == settings.check_url)
            .unwrap_or(PRESETS.len());
        let Some(index) = self.select(tui!(self.language, choose_target), &choices, default)?
        else {
            return Ok(false);
        };
        let selected = if index < PRESETS.len() {
            PRESETS[index].1.to_owned()
        } else {
            self.secret(tui!(self.language, check_url), &settings.check_url)?
        };
        let period = self.positive(
            tui!(self.language, active_interval),
            settings.interval_seconds,
            86400,
        )?;
        let reserve_period = self.positive(
            tui!(self.language, reserve_interval),
            settings.reserve_interval_seconds,
            86400,
        )?;
        let threshold = self.positive(
            tui!(self.language, failure_threshold),
            settings.failure_threshold.into(),
            100,
        )? as u32;
        let timeout = self.positive(tui!(self.language, timeout), settings.timeout_seconds, 300)?;
        let refresh = self.positive(
            tui!(self.language, refresh_interval),
            settings.refresh_seconds,
            2_592_000,
        )?;
        let mut diagnostics = settings.diagnostic_urls.clone();
        diagnostics.retain(|url| url != &selected);
        if self.confirm(tui!(self.language, choose_diagnostics), false)? {
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
            let names: Vec<_> = options
                .iter()
                .map(|url| target_label(self.language, url))
                .collect();
            let defaults: Vec<_> = options
                .iter()
                .map(|url| diagnostics.contains(url))
                .collect();
            self.line(tui!(self.language, diagnostics_hint))?;
            let Some(indices) = MultiSelect::with_theme(&self.theme)
                .with_prompt(tui!(self.language, diagnostics))
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
                && self.confirm(tui!(self.language, add_diagnostic), false)?
            {
                let url = self.secret(tui!(self.language, diagnostic_url), "")?;
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
        let choices = [
            tui!(self.language, telegram_route),
            tui!(self.language, smtp_route),
        ];
        self.line(tui!(self.language, channel_hint))?;
        let Some(selected) = MultiSelect::with_theme(&self.theme)
            .with_prompt(tui!(self.language, channels))
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
                    tui!(self.language, telegram_token),
                    existing.map_or("", |t| t.token.as_str()),
                )?,
                chat_id: self.text(
                    tui!(self.language, telegram_chat_id),
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
                tui!(self.language, smtp_host),
                existing.map_or("", |s| s.host.as_str()),
                false,
            )?;
            let port = self.positive(
                tui!(self.language, smtp_port),
                existing.map_or(465, |s| u64::from(s.port)),
                u16::MAX.into(),
            )? as u16;
            let username = self.text(
                tui!(self.language, smtp_username),
                existing.map_or("", |s| s.username.as_str()),
                true,
            )?;
            let password = if username.is_empty() {
                String::new()
            } else {
                self.secret(
                    tui!(self.language, smtp_password),
                    existing.map_or("", |s| s.password.as_str()),
                )?
            };
            Some(Smtp {
                host,
                port,
                username,
                password,
                from: self.text(
                    tui!(self.language, sender),
                    existing.map_or("", |s| s.from.as_str()),
                    false,
                )?,
                to: self.text(
                    tui!(self.language, recipient),
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
            self.line(tui!(self.language, no_state))?;
            return Ok(());
        }
        let status = match state.status.as_str() {
            "starting" => tui!(self.language, starting),
            "healthy" => tui!(self.language, healthy),
            "degraded" => tui!(self.language, degraded),
            "outage" => tui!(self.language, outage),
            "stopped" => tui!(self.language, stopped),
            _ => tui!(self.language, unknown),
        };
        self.line(&tui!(
            self.language,
            service_status,
            status,
            elapsed_label(self.language, now(), state.updated_at)
        ))?;
        self.line(&tui!(
            self.language,
            node_count,
            state
                .nodes
                .values()
                .filter(|health| health.available == Some(true))
                .count(),
            state.nodes.len()
        ))?;
        if let Some(preferred) = state.preferred.as_deref() {
            self.line(&tui!(
                self.language,
                preferred_node,
                display_text(preferred)
            ))?;
        }
        self.line(&tui!(
            self.language,
            active_node,
            state
                .active
                .as_deref()
                .map(display_text)
                .unwrap_or_else(|| tui!(self.language, no_active).into())
        ))?;
        self.line(&tui!(
            self.language,
            check_result,
            target_label(self.language, &settings.check_url),
            state
                .last_probe
                .map_or(tui!(self.language, pending), |passed| probe_label(
                    self.language,
                    passed
                ))
        ))?;
        for (target, passed) in &state.diagnostics {
            self.line(&tui!(
                self.language,
                diagnostic_result,
                target_label(self.language, target),
                probe_label(self.language, *passed)
            ))?;
        }
        for reason in &state.skipped {
            self.line(&tui!(
                self.language,
                skipped_entry,
                display_text(&reason.text(self.language))
            ))?;
        }
        Ok(())
    }

    fn nodes(&self, state: &State) -> Result<()> {
        if state.nodes.is_empty() {
            self.screen(tui!(self.language, nodes))?;
            self.line(tui!(self.language, no_probes))?;
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
        choices.push(tui!(self.language, back).into());
        let mut selected = 0;
        loop {
            self.screen(tui!(self.language, nodes))?;
            self.line(tui!(self.language, node_legend))?;
            let Some(index) = self.select(tui!(self.language, choose_node), &choices, selected)?
            else {
                return Ok(());
            };
            if index == entries.len() {
                return Ok(());
            }
            selected = index;
            let (name, health) = entries[index];
            self.screen(&display_text(name))?;
            let status = match health.available {
                Some(true) => tui!(self.language, available),
                Some(false) => tui!(self.language, unavailable),
                None => tui!(self.language, unchecked),
            };
            self.line(&tui!(self.language, node_status, status))?;
            if state.active.as_ref() == Some(name) {
                self.line(tui!(self.language, active_route))?;
            }
            if state.preferred.as_ref() == Some(name) {
                self.line(tui!(self.language, preferred_route))?;
            }
            if health.since != 0 {
                self.line(&tui!(
                    self.language,
                    state_since,
                    timestamp(self.language, health.since)
                ))?;
            }
            if health.checked_at != 0 {
                self.line(&tui!(
                    self.language,
                    last_checked,
                    timestamp(self.language, health.checked_at)
                ))?;
            }
            if let Some(latency) = health.latency_ms {
                self.line(&tui!(self.language, latency, latency))?;
            }
            self.pause()?;
        }
    }

    fn history(&self, settings: &Settings, state: &State) -> Result<()> {
        self.screen(tui!(self.language, history))?;
        if state.history.is_empty() {
            self.line(tui!(self.language, no_events))?;
        } else {
            let events: Vec<_> = state.history.iter().rev().collect();
            let time = now();
            let mut labels: Vec<_> = events
                .iter()
                .map(|event| {
                    format!(
                        "{} · {}",
                        event_label(self.language, &event.kind)
                            .unwrap_or(tui!(self.language, event)),
                        elapsed_label(self.language, time, event.at)
                    )
                })
                .collect();
            labels.push(tui!(self.language, back).into());
            let mut selected = 0;
            loop {
                self.screen(tui!(self.language, history))?;
                let Some(index) =
                    self.select(tui!(self.language, choose_event), &labels, selected)?
                else {
                    return Ok(());
                };
                if index == events.len() {
                    return Ok(());
                }
                selected = index;
                let event = events[index];
                self.screen(tui!(self.language, event_details))?;
                let alert = Alert::from_event(settings, event, false)?;
                self.line(&alert.text)?;
                for (name, available) in event.changes.iter().skip(5) {
                    self.line(&format!(
                        "{}: {}",
                        display_text(name),
                        if *available {
                            tui!(self.language, available)
                        } else {
                            tui!(self.language, unavailable)
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

fn target_label(language: Language, url: &str) -> String {
    if let Some((name, _)) = PRESETS.iter().find(|(_, address)| *address == url) {
        return (*name).to_owned();
    }
    Url::parse(url)
        .ok()
        .and_then(|address| address.host_str().map(display_text))
        .unwrap_or_else(|| tui!(language, custom_url).to_owned())
}

fn enabled(language: Language, configured: bool) -> &'static str {
    if configured {
        tui!(language, enabled)
    } else {
        tui!(language, disabled)
    }
}

fn probe_label(language: Language, passed: bool) -> &'static str {
    if passed {
        tui!(language, available)
    } else {
        tui!(language, failed)
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn elapsed_label(language: Language, now: u64, timestamp: u64) -> String {
    match now.saturating_sub(timestamp) {
        0..=4 => tui!(language, just_now).to_owned(),
        seconds @ 5..=59 => tui!(language, seconds_ago, seconds),
        seconds @ 60..=3599 => tui!(language, minutes_ago, seconds / 60),
        seconds @ 3600..=86399 => tui!(language, hours_ago, seconds / 3600),
        seconds => tui!(language, days_ago, seconds / 86400),
    }
}

#[cfg(test)]
#[path = "../tests/unit/interface.rs"]
mod tests;
