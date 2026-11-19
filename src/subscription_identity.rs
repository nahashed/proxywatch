use crate::runtime_text::message;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum HwidMode {
    Toggle(bool),
    Custom(String),
}

impl Default for HwidMode {
    fn default() -> Self {
        Self::Toggle(false)
    }
}

impl HwidMode {
    pub fn enabled(&self) -> bool {
        matches!(self, Self::Toggle(true) | Self::Custom(_))
    }

    pub fn validate(&self) -> Result<()> {
        if let Self::Custom(value) = self {
            ensure!(
                (10..=64).contains(&value.len())
                    && value
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'=' | b'-')),
                message!(invalid_custom_hwid)
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_accepts_disabled_automatic_and_custom_modes() -> serde_json::Result<()> {
        for (json, expected) in [
            ("false", HwidMode::Toggle(false)),
            ("true", HwidMode::Toggle(true)),
            (r#""cluster-main""#, HwidMode::Custom("cluster-main".into())),
        ] {
            let mode: HwidMode = serde_json::from_str(json)?;
            assert_eq!(mode, expected);
            assert_eq!(serde_json::to_string(&mode)?, json);
        }
        Ok(())
    }
}
