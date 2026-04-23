//! Compression level knob — consumed by the agent runner on every tool call.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum CompressionLevel {
    #[default]
    Off,
    Minimal,
    Aggressive,
}

impl CompressionLevel {
    pub fn is_off(&self) -> bool {
        matches!(self, CompressionLevel::Off)
    }
}

impl FromStr for CompressionLevel {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" | "none" | "false" | "0" => Ok(CompressionLevel::Off),
            "on" | "min" | "minimal" | "true" | "1" => Ok(CompressionLevel::Minimal),
            "aggr" | "aggressive" | "max" => Ok(CompressionLevel::Aggressive),
            other => Err(format!("unknown compression level: {other:?}")),
        }
    }
}

impl fmt::Display for CompressionLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CompressionLevel::Off => write!(f, "off"),
            CompressionLevel::Minimal => write!(f, "minimal"),
            CompressionLevel::Aggressive => write!(f, "aggressive"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_aliases() {
        assert_eq!("off".parse(), Ok(CompressionLevel::Off));
        assert_eq!("on".parse(), Ok(CompressionLevel::Minimal));
        assert_eq!("min".parse(), Ok(CompressionLevel::Minimal));
        assert_eq!("aggressive".parse(), Ok(CompressionLevel::Aggressive));
        assert_eq!("max".parse(), Ok(CompressionLevel::Aggressive));
        assert!("bogus".parse::<CompressionLevel>().is_err());
    }

    #[test]
    fn roundtrip_display() {
        for lvl in [
            CompressionLevel::Off,
            CompressionLevel::Minimal,
            CompressionLevel::Aggressive,
        ] {
            let s = lvl.to_string();
            assert_eq!(s.parse::<CompressionLevel>().unwrap(), lvl);
        }
    }
}
