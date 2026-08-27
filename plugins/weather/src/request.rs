//! Parse + validate weather requests.

pub const MAX_TIMEOUT_MS: u64 = 30_000;
pub const DEFAULT_TIMEOUT_MS: u64 = 10_000;
pub const DEFAULT_DAYS: u32 = 3;
pub const MAX_DAYS: u32 = 16;

#[derive(Debug, Clone)]
pub struct WeatherNowParams {
    pub lat: f64,
    pub lon: f64,
    pub timezone: String,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone)]
pub struct WeatherForecastParams {
    pub lat: f64,
    pub lon: f64,
    pub days: u32,
    pub timezone: String,
    pub timeout_ms: u64,
}

fn validate_lat_lon(lat: f64, lon: f64) -> Result<(), String> {
    if !(-90.0..=90.0).contains(&lat) {
        return Err(format!("lat out of range -90..90: {lat}"));
    }
    if !(-180.0..=180.0).contains(&lon) {
        return Err(format!("lon out of range -180..180: {lon}"));
    }
    if !lat.is_finite() || !lon.is_finite() {
        return Err("lat/lon must be finite numbers".into());
    }
    Ok(())
}

pub fn parse_now(params_json: &[u8]) -> Result<WeatherNowParams, String> {
    #[derive(serde::Deserialize)]
    struct Raw {
        lat: Option<serde_json::Value>,
        lon: Option<serde_json::Value>,
        timezone: Option<String>,
        timeout_ms: Option<u64>,
    }
    let raw: Raw = serde_json::from_slice(params_json).map_err(|e| format!("invalid JSON: {e}"))?;
    let lat = raw
        .lat
        .ok_or("missing required field: lat")?
        .as_f64()
        .ok_or("lat must be a number")?;
    let lon = raw
        .lon
        .ok_or("missing required field: lon")?
        .as_f64()
        .ok_or("lon must be a number")?;
    validate_lat_lon(lat, lon)?;
    let timezone = raw.timezone.unwrap_or_else(|| "auto".into());
    if timezone.trim().is_empty() {
        return Err("timezone must not be empty".into());
    }
    let timeout_ms = raw.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS).min(MAX_TIMEOUT_MS);
    if timeout_ms == 0 {
        return Err("timeout_ms must be > 0".into());
    }
    Ok(WeatherNowParams {
        lat,
        lon,
        timezone,
        timeout_ms,
    })
}

pub fn parse_forecast(params_json: &[u8]) -> Result<WeatherForecastParams, String> {
    #[derive(serde::Deserialize)]
    struct Raw {
        lat: Option<serde_json::Value>,
        lon: Option<serde_json::Value>,
        days: Option<u32>,
        timezone: Option<String>,
        timeout_ms: Option<u64>,
    }
    let raw: Raw = serde_json::from_slice(params_json).map_err(|e| format!("invalid JSON: {e}"))?;
    let lat = raw
        .lat
        .ok_or("missing required field: lat")?
        .as_f64()
        .ok_or("lat must be a number")?;
    let lon = raw
        .lon
        .ok_or("missing required field: lon")?
        .as_f64()
        .ok_or("lon must be a number")?;
    validate_lat_lon(lat, lon)?;
    let days = raw.days.unwrap_or(DEFAULT_DAYS);
    if !(1..=MAX_DAYS).contains(&days) {
        return Err(format!("days out of range 1..{MAX_DAYS}: {days}"));
    }
    let timezone = raw.timezone.unwrap_or_else(|| "auto".into());
    if timezone.trim().is_empty() {
        return Err("timezone must not be empty".into());
    }
    let timeout_ms = raw.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS).min(MAX_TIMEOUT_MS);
    if timeout_ms == 0 {
        return Err("timeout_ms must be > 0".into());
    }
    Ok(WeatherForecastParams {
        lat,
        lon,
        days,
        timezone,
        timeout_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_accepts_valid() {
        let p = parse_now(br#"{"lat": 52.5, "lon": 13.4}"#).unwrap();
        assert_eq!(p.lat, 52.5);
        assert_eq!(p.timezone, "auto");
    }

    #[test]
    fn now_rejects_bad_lat() {
        let e = parse_now(br#"{"lat": 100, "lon": 0}"#).unwrap_err();
        assert!(e.contains("lat"), "{e}");
    }

    #[test]
    fn forecast_clamps_days() {
        let p = parse_forecast(br#"{"lat": 0, "lon": 0, "days": 5}"#).unwrap();
        assert_eq!(p.days, 5);
    }

    #[test]
    fn forecast_rejects_days_oob() {
        let e = parse_forecast(br#"{"lat": 0, "lon": 0, "days": 20}"#).unwrap_err();
        assert!(e.contains("days"), "{e}");
    }

    #[test]
    fn missing_lat() {
        let e = parse_now(br#"{"lon": 0}"#).unwrap_err();
        assert!(e.contains("lat"), "{e}");
    }
}
