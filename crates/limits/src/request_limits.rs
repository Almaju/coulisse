use std::collections::HashMap;

use crate::LimitError;

const KEY_DAY: &str = "tokens_per_day";
const KEY_HOUR: &str = "tokens_per_hour";
const KEY_MONTH: &str = "tokens_per_month";

#[derive(Clone, Copy, Debug, Default)]
pub struct RequestLimits {
    pub tokens_per_day: Option<u64>,
    pub tokens_per_hour: Option<u64>,
    pub tokens_per_month: Option<u64>,
}

impl RequestLimits {
    pub fn from_metadata(metadata: &HashMap<String, String>) -> Result<Self, LimitError> {
        let parse = |key: &str| -> Result<Option<u64>, LimitError> {
            metadata
                .get(key)
                .map(|v| {
                    v.parse::<u64>().map_err(|_| LimitError::InvalidMetadata {
                        key: key.into(),
                        value: v.clone(),
                    })
                })
                .transpose()
        };
        Ok(Self {
            tokens_per_day: parse(KEY_DAY)?,
            tokens_per_hour: parse(KEY_HOUR)?,
            tokens_per_month: parse(KEY_MONTH)?,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.tokens_per_day.is_none()
            && self.tokens_per_hour.is_none()
            && self.tokens_per_month.is_none()
    }
}
