//! Effort levels: control thinking budget and temperature.

use serde::{Deserialize, Serialize};

/// Effort level controlling thinking depth and quality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EffortLevel {
    /// Minimal thinking, fast responses.
    Low,
    /// Moderate thinking (default).
    Medium,
    /// Deep thinking for complex tasks.
    High,
    /// Maximum thinking budget.
    Max,
}

impl EffortLevel {
    /// Thinking budget in tokens for this effort level.
    pub fn thinking_budget_tokens(&self) -> u32 {
        match self {
            Self::Low => 1024,
            Self::Medium => 4096,
            Self::High => 8192,
            Self::Max => 32768,
        }
    }

    /// Temperature override for this effort level.
    pub fn temperature(&self) -> Option<f32> {
        match self {
            Self::Low => Some(0.3),
            Self::Medium => None, // use default
            Self::High => None,
            Self::Max => Some(1.0),
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "low" | "min" => Self::Low,
            "medium" | "med" | "default" => Self::Medium,
            "high" => Self::High,
            "max" | "maximum" => Self::Max,
            _ => Self::Medium,
        }
    }
}

impl Default for EffortLevel {
    fn default() -> Self {
        Self::Medium
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_thinking_budget() {
        assert!(
            EffortLevel::Low.thinking_budget_tokens() < EffortLevel::High.thinking_budget_tokens()
        );
        assert!(
            EffortLevel::High.thinking_budget_tokens() < EffortLevel::Max.thinking_budget_tokens()
        );
    }

    #[test]
    fn test_from_str() {
        assert_eq!(EffortLevel::from_str("low"), EffortLevel::Low);
        assert_eq!(EffortLevel::from_str("HIGH"), EffortLevel::High);
        assert_eq!(EffortLevel::from_str("max"), EffortLevel::Max);
        assert_eq!(EffortLevel::from_str("unknown"), EffortLevel::Medium);
    }

    #[test]
    fn test_temperature() {
        assert!(EffortLevel::Low.temperature().is_some());
        assert!(EffortLevel::Medium.temperature().is_none()); // default
    }
}
