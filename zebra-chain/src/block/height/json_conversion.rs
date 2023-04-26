//! Consensus-critical conversion from JSON [`Value`] to [`Height`].

use serde_json::Value;

use crate::BoxError;

use super::{Height, TryIntoHeight};

impl TryIntoHeight for Value {
    type Error = BoxError;

    fn try_into_height(&self) -> Result<Height, Self::Error> {
        if self.is_number() {
            let height = self.as_u64().ok_or("JSON value outside u64 range")?;
            return height.try_into_height();
        }

        if let Some(height) = self.as_str() {
            return height.try_into_height();
        }

        Err("JSON value must be a number or string".into())
    }
}
