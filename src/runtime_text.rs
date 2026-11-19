#[path = "../locales/en/runtime.rs"]
pub(crate) mod en;
#[path = "../locales/ru/runtime.rs"]
pub(crate) mod ru;

macro_rules! message {
    ($key:ident) => {
        match $crate::i18n::language() {
            $crate::i18n::Language::En => $crate::runtime_text::en::text!($key),
            $crate::i18n::Language::Ru => $crate::runtime_text::ru::text!($key),
        }
    };
    ($key:ident, $($argument:expr),+ $(,)?) => {
        match $crate::i18n::language() {
            $crate::i18n::Language::En => format!($crate::runtime_text::en::text!($key), $($argument),+),
            $crate::i18n::Language::Ru => format!($crate::runtime_text::ru::text!($key), $($argument),+),
        }
    };
}
pub(crate) use message;

// Keep both safe descriptions so a saved subscription can be viewed in either language.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct NodeError {
    pub en: String,
    pub ru: String,
}

pub type NodeResult<T> = Result<T, NodeError>;

impl NodeError {
    pub fn text(&self, language: crate::i18n::Language) -> &str {
        match language {
            crate::i18n::Language::En => &self.en,
            crate::i18n::Language::Ru => &self.ru,
        }
    }
}

impl std::fmt::Display for NodeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.text(crate::i18n::language()))
    }
}

impl std::error::Error for NodeError {}

macro_rules! node_error {
    ($key:ident) => {
        $crate::runtime_text::NodeError {
            en: $crate::runtime_text::en::text!($key).to_owned(),
            ru: $crate::runtime_text::ru::text!($key).to_owned(),
        }
    };
    ($key:ident, $($argument:expr),+ $(,)?) => {
        $crate::runtime_text::NodeError {
            en: format!($crate::runtime_text::en::text!($key) $(, $argument)*),
            ru: format!($crate::runtime_text::ru::text!($key) $(, $argument)*),
        }
    };
}
pub(crate) use node_error;

macro_rules! require {
    ($condition:expr, $key:ident $(, $argument:expr)* $(,)?) => {
        if !($condition) {
            return Err($crate::runtime_text::node_error!($key $(, $argument)*));
        }
    };
}
pub(crate) use require;
