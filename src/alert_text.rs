use crate::i18n::Language;

pub struct AlertText {
    pub delayed: &'static str,
    pub unknown_kind: &'static str,
    pub instance: &'static str,
    pub server_time: &'static str,
    pub purpose: &'static str,
    pub unspecified: &'static str,
    pub up: &'static str,
    pub down: &'static str,
    pub unnamed: &'static str,
    pub more: &'static str,
    pub stopped: &'static str,
    pub route: &'static str,
    pub available: &'static str,
    pub of: &'static str,
    pub event: &'static str,
    pub start: &'static str,
    pub snapshot: &'static str,
    pub nodes: &'static str,
    pub switch: &'static str,
    pub failback: &'static str,
    pub outage: &'static str,
    pub recovery: &'static str,
    pub unknown_time: &'static str,
    pub no_delivery: &'static str,
    pub token: &'static str,
    pub unreachable: &'static str,
    pub rejected: &'static str,
    pub receipt: &'static str,
    pub unconfirmed: &'static str,
    pub sender: &'static str,
    pub recipient: &'static str,
    pub compose: &'static str,
    pub tls: &'static str,
    pub delivery: &'static str,
}

pub fn alerts(language: Language) -> &'static AlertText {
    match language {
        Language::En => &include!("../locales/en/alerts.rs"),
        Language::Ru => &include!("../locales/ru/alerts.rs"),
    }
}
