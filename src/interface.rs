use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Result, ensure};
use console::{Term, style, truncate_str};
use dialoguer::{Input, MultiSelect, Password, Select, theme::ColorfulTheme};
use reqwest::Url;

use crate::{
    i18n::Language,
    notifications::{Alert, event_label, timestamp},
    storage::{
        DiagnosticResource, MAX_DIAGNOSTIC_INTERVAL_SECONDS, PRESETS, Paths, Settings, Smtp, State,
        Telegram,
    },
    subscription_identity::HwidMode,
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

pub fn run(paths: &Paths, explicit_directory: bool) -> Result<()> {
    let terminal = Term::stderr();
    let mut ui = Interface {
        installer_setup: !explicit_directory
            && std::env::var_os("PROXYWATCH_INSTALL_SETUP").is_some_and(|value| value == "1"),
        explicit_directory,
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
        if !ui.setup(paths)? || ui.installer_setup {
            return Ok(());
        }
    }
    loop {
        let settings = paths.load_settings()?;
        ui.set_language(settings.language);
        ui.screen("Proxywatch")?;
        let state = paths.load_state()?;
        ui.status(&settings, &state)?;
        let mut actions = vec![
            tui!(ui.language, refresh),
            tui!(ui.language, nodes),
            tui!(ui.language, history),
            tui!(ui.language, settings),
        ];
        let listeners = if crate::handover::available(paths, explicit_directory)
            && !crate::handover::conflicts(&settings).is_empty()
        {
            crate::xray_listeners::inspect(settings.http_port, settings.socks_port)
                .filter(|listeners| !listeners.busy)
        } else {
            None
        };
        if listeners.is_some() {
            actions.push(tui!(ui.language, hand_over_ports));
        }
        actions.push(tui!(ui.language, close));
        match ui.select(tui!(ui.language, action), &actions, 0)? {
            Some(0) => (),
            Some(1) => ui.nodes(&state)?,
            Some(2) => ui.history(&settings, &state)?,
            Some(3) => {
                ui.edit(paths, settings, false)?;
            }
            Some(4) => {
                let Some(listeners) = listeners else {
                    return Ok(());
                };
                ui.handover(paths, &settings, &listeners, Some(&settings))?;
            }
            _ => return Ok(()),
        }
    }
}

enum PortAction {
    Retry,
    Edit,
    Applied,
}

struct Interface {
    installer_setup: bool,
    explicit_directory: bool,
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
        self.number(title, current, 1, maximum)
    }

    fn number(&self, title: &str, current: u64, minimum: u64, maximum: u64) -> Result<u64> {
        let error = || tui!(self.language, number_range, minimum, maximum);
        let input = Input::<String>::with_theme(&self.theme)
            .with_prompt(title)
            .default(current.to_string())
            .validate_with(|text: &String| {
                text.parse::<u64>()
                    .ok()
                    .filter(|value| (minimum..=maximum).contains(value))
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
            xray_binary: crate::xray_binary::setup_binary(),
            ..Settings::default()
        };
        self.subscription(&mut settings)?;
        self.identity(&mut settings)?;
        if !self.ports(&mut settings)? || !self.checks(&mut settings)? {
            return Ok(false);
        }
        self.line(tui!(self.language, notification_intro))?;
        self.notifications(&mut settings)?;
        self.edit(paths, settings, true)
    }

    fn edit(&mut self, paths: &Paths, mut settings: Settings, first_setup: bool) -> Result<bool> {
        loop {
            self.screen(tui!(self.language, settings_title))?;
            self.line(&display_text(&settings.instance_name))?;
            self.line(&ports_label(settings.http_port, settings.socks_port))?;
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
                    if self.save(paths, &mut settings, first_setup)? {
                        return Ok(true);
                    }
                }
                Some(1) => self.subscription(&mut settings)?,
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

    fn save(&self, paths: &Paths, settings: &mut Settings, first_setup: bool) -> Result<bool> {
        use crate::user_service::{Applied, UserService, foreground_command};

        if first_setup {
            loop {
                let conflicts = crate::handover::conflicts(settings);
                if conflicts.is_empty() {
                    break;
                }
                match self.resolve_port_conflict(paths, settings, &conflicts)? {
                    PortAction::Retry => continue,
                    PortAction::Edit => return Ok(false),
                    PortAction::Applied => return Ok(true),
                }
            }
        }

        let service = UserService::installed(paths.directory(), self.explicit_directory);
        let xray_ready = self.resolve_xray(settings)?;
        if !xray_ready && service.is_some() {
            self.pause()?;
            return Ok(false);
        }
        if first_setup {
            paths.try_first_settings(settings, || Ok(()))?;
        } else {
            paths.save_settings(settings)?;
        }
        let outcome = service.map(|service| service.apply(first_setup));
        match outcome {
            Some(Applied::Started) => self.line(tui!(self.language, service_started))?,
            Some(Applied::Restarted) => self.line(tui!(self.language, service_restarted))?,
            Some(Applied::Failed) => self.line(tui!(self.language, service_failed))?,
            None if self.installer_setup && first_setup && xray_ready => {
                self.line(tui!(self.language, saved_without_service))?;
                if self.confirm(tui!(self.language, run_in_terminal), false)? {
                    use std::os::unix::process::CommandExt;
                    let error = std::process::Command::new(std::env::current_exe()?)
                        .arg("run")
                        .exec();
                    return Err(error.into());
                }
                self.line(&tui!(self.language, run_later, foreground_command(None)))?;
            }
            Some(Applied::Inactive) | None => {
                let command =
                    foreground_command(self.explicit_directory.then_some(paths.directory()));
                self.line(&tui!(self.language, saved_foreground, command))?;
            }
        }
        if !self.installer_setup || !first_setup {
            self.pause()?;
        }
        Ok(true)
    }

    fn resolve_xray(&self, settings: &mut Settings) -> Result<bool> {
        let Some((binary, [major, minor, patch])) =
            crate::xray_binary::inspect_binary(&settings.xray_binary)
        else {
            self.line(tui!(self.language, xray_unavailable))?;
            return Ok(false);
        };
        // The service manager can have a different PATH from the interactive shell.
        if let Some(binary) = binary.to_str() {
            settings.xray_binary = binary.to_owned();
        }
        self.line(&tui!(self.language, xray_detected, major, minor, patch))?;
        Ok(true)
    }

    fn resolve_port_conflict(
        &self,
        paths: &Paths,
        settings: &mut Settings,
        conflicts: &[u16],
    ) -> Result<PortAction> {
        self.screen(tui!(self.language, ports_in_use))?;
        for port in conflicts {
            self.line(&tui!(self.language, occupied_address, port))?;
        }
        let mut choices = vec![
            tui!(self.language, change_ports),
            tui!(self.language, retry_ports),
            tui!(self.language, return_to_settings),
        ];
        let inspected = if crate::handover::available(paths, self.explicit_directory) {
            let inspected = crate::xray_listeners::inspect(settings.http_port, settings.socks_port);
            if let Some(listeners) = &inspected {
                self.line(&tui!(self.language, detected_listeners, listeners.pid))?;
                self.line(&ports_label(listeners.http, listeners.socks))?;
                if listeners.busy {
                    self.line(tui!(self.language, listeners_busy))?;
                } else {
                    choices.push(tui!(self.language, test_handover));
                }
            } else {
                self.line(tui!(self.language, unknown_listeners))?;
            }
            inspected
        } else {
            None
        };
        match self.select(tui!(self.language, action), &choices, 0)? {
            Some(0) => {
                self.ports(settings)?;
                Ok(PortAction::Edit)
            }
            Some(1) => Ok(PortAction::Retry),
            Some(3) => {
                if let Some(listeners) = inspected.filter(|item| !item.busy) {
                    return self.handover(paths, settings, &listeners, None);
                }
                Ok(PortAction::Edit)
            }
            _ => Ok(PortAction::Edit),
        }
    }

    fn handover(
        &self,
        paths: &Paths,
        settings: &Settings,
        listeners: &crate::xray_listeners::XrayListeners,
        expected_settings: Option<&Settings>,
    ) -> Result<PortAction> {
        use crate::handover::{StartMode, cutover, verify_route};
        use crate::user_service::UserService;

        let mut candidate = settings.clone();
        candidate.http_port = listeners.http;
        candidate.socks_port = listeners.socks;
        if !self.resolve_xray(&mut candidate)? {
            self.pause()?;
            return Ok(PortAction::Edit);
        }
        self.line(tui!(self.language, checking_handover))?;
        if let Err(error) = verify_route(&candidate) {
            self.line(&error.to_string())?;
            self.pause()?;
            return Ok(PortAction::Edit);
        }
        self.line(tui!(self.language, route_verified))?;
        self.line(&ports_label(candidate.http_port, candidate.socks_port))?;
        let user_service =
            UserService::installed(paths.directory(), self.explicit_directory).is_some();
        let mode = if user_service {
            self.line(tui!(self.language, handover_user_service))?;
            StartMode::UserService
        } else {
            self.line(tui!(self.language, handover_foreground))?;
            StartMode::Foreground
        };
        self.line(tui!(self.language, rollback_notice))?;
        if !self.confirm(tui!(self.language, confirm_handover), false)? {
            return Ok(PortAction::Edit);
        }
        if let Err(error) = cutover(paths, &candidate, listeners, mode, expected_settings) {
            self.line(&error.to_string())?;
            self.pause()?;
            return Ok(PortAction::Edit);
        }
        if user_service {
            self.line(tui!(self.language, handover_started))?;
        }
        if !self.installer_setup {
            self.pause()?;
        }
        Ok(PortAction::Applied)
    }

    fn subscription(&self, settings: &mut Settings) -> Result<()> {
        let subscription_url = self.secret(
            tui!(self.language, subscription_url),
            &settings.subscription_url,
        )?;
        let default = match settings.subscription_hwid {
            HwidMode::Toggle(false) => 0,
            HwidMode::Toggle(true) => 1,
            HwidMode::Custom(_) => 2,
        };
        let Some(choice) = self.select(
            tui!(self.language, subscription_hwid),
            &[
                tui!(self.language, hwid_off),
                tui!(self.language, hwid_automatic),
                tui!(self.language, hwid_custom),
            ],
            default,
        )?
        else {
            return Ok(());
        };
        let subscription_hwid = match choice {
            0 => HwidMode::Toggle(false),
            1 => HwidMode::Toggle(true),
            2 => {
                self.line(tui!(self.language, hwid_custom_notice))?;
                let current = match &settings.subscription_hwid {
                    HwidMode::Custom(value) => value.as_str(),
                    HwidMode::Toggle(_) => "",
                };
                HwidMode::Custom(self.secret(tui!(self.language, hwid_custom_value), current)?)
            }
            _ => unreachable!(),
        };
        settings.subscription_url = subscription_url;
        settings.subscription_hwid = subscription_hwid;
        Ok(())
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
        let Some(diagnostics) =
            self.diagnostic_resources(&settings.diagnostic_urls, &selected, period)?
        else {
            return Ok(false);
        };
        settings.check_url = selected;
        settings.interval_seconds = period;
        settings.reserve_interval_seconds = reserve_period;
        settings.failure_threshold = threshold;
        settings.timeout_seconds = timeout;
        settings.refresh_seconds = refresh;
        settings.diagnostic_urls = diagnostics;
        Ok(true)
    }

    fn diagnostic_resources(
        &self,
        current: &[DiagnosticResource],
        primary: &str,
        main_interval: u64,
    ) -> Result<Option<Vec<DiagnosticResource>>> {
        let current: Vec<_> = current
            .iter()
            .filter(|item| item.url() != primary)
            .collect();
        if !self.confirm(tui!(self.language, choose_diagnostics), false)? {
            return Ok(Some(current.into_iter().cloned().collect()));
        }
        let mut options: Vec<_> = PRESETS
            .iter()
            .map(|(_, url)| *url)
            .filter(|url| *url != primary)
            .collect();
        let mut names: Vec<_> = options
            .iter()
            .map(|url| target_label(self.language, url))
            .collect();
        for (index, item) in current.iter().enumerate() {
            if !options.contains(&item.url()) {
                options.push(item.url());
                names.push(tui!(
                    self.language,
                    saved_resource,
                    index + 1,
                    target_label(self.language, item.url())
                ));
            }
        }
        let mut defaults: Vec<_> = options
            .iter()
            .map(|url| current.iter().any(|item| item.url() == *url))
            .collect();
        self.line(tui!(self.language, diagnostics_hint))?;
        let indices = loop {
            let Some(indices) = MultiSelect::with_theme(&self.theme)
                .with_prompt(tui!(self.language, diagnostics))
                .items(&names)
                .defaults(&defaults)
                .interact_on_opt(&self.terminal)?
            else {
                return Ok(None);
            };
            if indices.len() <= 16 {
                break indices;
            }
            defaults.fill(false);
            for index in indices {
                defaults[index] = true;
            }
            self.line(tui!(self.language, diagnostic_limit))?;
        };
        let mut urls: Vec<String> = indices
            .into_iter()
            .map(|index| options[index].to_owned())
            .collect();
        while urls.len() < 16 && self.confirm(tui!(self.language, add_diagnostic), false)? {
            let url = self.secret(tui!(self.language, diagnostic_url), "")?;
            if url == primary || urls.contains(&url) {
                self.line(tui!(self.language, diagnostic_duplicate))?;
            } else {
                urls.push(url);
            }
        }
        if !urls.is_empty() {
            self.line(&tui!(
                self.language,
                diagnostic_schedule_hint,
                main_interval
            ))?;
        }
        let mut resources = Vec::with_capacity(urls.len());
        for (index, url) in urls.into_iter().enumerate() {
            let interval = current
                .iter()
                .find(|item| item.url() == url)
                .and_then(|item| item.interval_seconds())
                .unwrap_or(0);
            let title = tui!(
                self.language,
                diagnostic_interval,
                index + 1,
                target_label(self.language, &url)
            );
            let period = self.number(&title, interval, 0, MAX_DIAGNOSTIC_INTERVAL_SECONDS)?;
            resources.push(match period {
                0 => DiagnosticResource::Url(url),
                interval_seconds => DiagnosticResource::Scheduled {
                    url,
                    interval_seconds,
                },
            });
        }
        Ok(Some(resources))
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
        self.line(&ports_label(settings.http_port, settings.socks_port))?;
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
                self.line(&alert.text())?;
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

fn ports_label(http: Option<u16>, socks: Option<u16>) -> String {
    let mut endpoints = Vec::new();
    if let Some(port) = socks {
        endpoints.push(format!("SOCKS5 127.0.0.1:{port}"));
    }
    if let Some(port) = http {
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
