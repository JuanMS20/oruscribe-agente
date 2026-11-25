use chrono::{DateTime, Datelike, FixedOffset, NaiveDate, TimeZone, Timelike, Utc};
use serde::{Deserialize, Serialize};

/// Lo que la app conserva tras iniciar sesión (`oauth.go:43-50`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Token {
    #[serde(default)]
    pub access_token: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub refresh_token: String,
    #[serde(default)]
    pub token_type: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub scope: String,
    #[serde(default = "zero_expiry")]
    pub expiry: DateTime<FixedOffset>,
}

impl Token {
    /// Un minuto de gracia para que una petición no compita con el vencimiento.
    /// El cero de Go (`0001-01-01T00:00:00Z`) significa "sin vencimiento".
    pub fn expired(&self, now: DateTime<FixedOffset>) -> bool {
        !is_zero(&self.expiry) && now + chrono::Duration::minutes(1) > self.expiry
    }
}

pub(crate) fn zero_expiry() -> DateTime<FixedOffset> {
    let naive = NaiveDate::from_ymd_opt(1, 1, 1)
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .unwrap_or_default();
    Utc.from_utc_datetime(&naive).fixed_offset()
}

fn is_zero(expiry: &DateTime<FixedOffset>) -> bool {
    let utc = expiry.with_timezone(&Utc);
    utc.year() == 1
        && utc.month() == 1
        && utc.day() == 1
        && utc.hour() == 0
        && utc.minute() == 0
        && utc.second() == 0
        && utc.nanosecond() == 0
}
