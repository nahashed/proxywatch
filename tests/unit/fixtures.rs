use crate::i18n::Language;
use std::collections::BTreeMap;

pub fn messages(language: Language) -> BTreeMap<String, String> {
    let fixture = match language {
        Language::En => include_str!("../fixtures/en/messages.json"),
        Language::Ru => include_str!("../fixtures/ru/messages.json"),
    };
    serde_json::from_str(fixture).expect("invalid message fixture")
}
