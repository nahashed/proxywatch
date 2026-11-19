use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    En,
    #[default]
    Ru,
}

use std::sync::atomic::{AtomicBool, Ordering};

static ENGLISH: AtomicBool = AtomicBool::new(false);

pub fn set_language(value: Language) {
    ENGLISH.store(matches!(value, Language::En), Ordering::Relaxed);
}

pub fn language() -> Language {
    if ENGLISH.load(Ordering::Relaxed) {
        Language::En
    } else {
        Language::Ru
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_language_as_a_stable_code() -> serde_json::Result<()> {
        for (language, code) in [(Language::En, r#""en""#), (Language::Ru, r#""ru""#)] {
            assert_eq!(serde_json::to_string(&language)?, code);
            assert_eq!(serde_json::from_str::<Language>(code)?, language);
        }
        Ok(())
    }
}
