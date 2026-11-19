use crate::i18n::{Language, language};

pub struct CliText {
    pub about: &'static str,
    pub usage: &'static str,
    pub options_value: &'static str,
    pub command_value: &'static str,
    pub directory_value: &'static str,
    pub help_topics: &'static str,
    pub commands: &'static str,
    pub options: &'static str,
    pub run: &'static str,
    pub tui: &'static str,
    pub configure: &'static str,
    pub status: &'static str,
    pub help_command: &'static str,
    pub help: &'static str,
    pub version: &'static str,
    pub directory: &'static str,
    pub stdin: &'static str,
    pub json: &'static str,
    pub error: &'static str,
    pub arguments: &'static str,
    pub read_input: &'static str,
    pub expected_object: &'static str,
    pub invalid_settings: &'static str,
    pub read_state: &'static str,
    pub write_status: &'static str,
}

pub fn text() -> &'static CliText {
    match language() {
        Language::Ru => &include!("../locales/ru/cli.rs"),
        Language::En => &include!("../locales/en/cli.rs"),
    }
}
