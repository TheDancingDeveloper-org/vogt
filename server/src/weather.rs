use std::sync::Arc;

use axum::{
    extract::{Query, State},
    Json,
};
use serde::{Deserialize, Serialize};

use crate::{
    app::AppState,
    config::WeatherLocation,
    error::{ApiError, Result},
};

const OPEN_METEO_FORECAST_URL: &str = "https://api.open-meteo.com/v1/forecast";

#[derive(Debug, Clone, Deserialize)]
pub struct WeatherQuery {
    #[serde(alias = "lat")]
    pub latitude: Option<f64>,
    #[serde(alias = "lon", alias = "lng")]
    pub longitude: Option<f64>,
    pub label: Option<String>,
    pub timezone: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeatherBrief {
    pub location: WeatherLocation,
    #[serde(with = "time::serde::rfc3339")]
    pub fetched_at: time::OffsetDateTime,
    pub provider: String,
    pub provider_url: String,
    pub timezone: Option<String>,
    pub timezone_abbreviation: Option<String>,
    pub current: Option<CurrentWeather>,
    pub days: Vec<DailyWeather>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CurrentWeather {
    pub time: String,
    pub weather_code: Option<i32>,
    pub summary: Option<String>,
    pub temperature_c: Option<f64>,
    pub apparent_temperature_c: Option<f64>,
    pub wind_speed_kmh: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyWeather {
    pub date: String,
    pub weather_code: Option<i32>,
    pub summary: Option<String>,
    pub temp_min_c: Option<f64>,
    pub temp_max_c: Option<f64>,
    pub precipitation_probability_pct: Option<i32>,
    pub precipitation_sum_mm: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct OpenMeteoResponse {
    timezone: Option<String>,
    timezone_abbreviation: Option<String>,
    current: Option<OpenMeteoCurrent>,
    daily: Option<OpenMeteoDaily>,
}

#[derive(Debug, Deserialize)]
struct OpenMeteoCurrent {
    time: String,
    weather_code: Option<i32>,
    temperature_2m: Option<f64>,
    apparent_temperature: Option<f64>,
    wind_speed_10m: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct OpenMeteoDaily {
    time: Vec<String>,
    weather_code: Option<Vec<Option<i32>>>,
    temperature_2m_max: Option<Vec<Option<f64>>>,
    temperature_2m_min: Option<Vec<Option<f64>>>,
    precipitation_probability_max: Option<Vec<Option<i32>>>,
    precipitation_sum: Option<Vec<Option<f64>>>,
}

pub async fn forecast(
    State(state): State<Arc<AppState>>,
    Query(query): Query<WeatherQuery>,
) -> Result<Json<WeatherBrief>> {
    let location = resolve_location(&query, state.config.weather_location.as_ref())?;
    Ok(Json(fetch_forecast(location).await?))
}

pub fn resolve_location(
    query: &WeatherQuery,
    configured: Option<&WeatherLocation>,
) -> Result<WeatherLocation> {
    match (query.latitude, query.longitude) {
        (Some(latitude), Some(longitude)) => {
            let loc = WeatherLocation {
                label: query
                    .label
                    .as_ref()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "Requested location".to_string()),
                latitude,
                longitude,
                timezone: query
                    .timezone
                    .as_ref()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty()),
            };
            validate_location(&loc)?;
            Ok(loc)
        }
        (None, None) => configured.cloned().ok_or_else(|| {
            ApiError::BadRequest(
                "weather location required: pass latitude/longitude or configure weather_location"
                    .into(),
            )
        }),
        _ => Err(ApiError::BadRequest(
            "both latitude and longitude are required".into(),
        )),
    }
}

pub async fn fetch_forecast(location: WeatherLocation) -> Result<WeatherBrief> {
    validate_location(&location)?;
    let timezone = location
        .timezone
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("auto");
    let client = reqwest::Client::new();
    let response = client
        .get(OPEN_METEO_FORECAST_URL)
        .query(&[
            ("latitude", location.latitude.to_string()),
            ("longitude", location.longitude.to_string()),
            ("current", "temperature_2m,apparent_temperature,weather_code,wind_speed_10m".to_string()),
            ("daily", "weather_code,temperature_2m_max,temperature_2m_min,precipitation_probability_max,precipitation_sum".to_string()),
            ("forecast_days", "3".to_string()),
            ("timezone", timezone.to_string()),
        ])
        .send()
        .await
        .map_err(|e| ApiError::Internal(format!("weather fetch failed: {e}")))?;

    if !response.status().is_success() {
        return Err(ApiError::Internal(format!(
            "weather provider returned HTTP {}",
            response.status()
        )));
    }

    let body = response
        .json::<OpenMeteoResponse>()
        .await
        .map_err(|e| ApiError::Internal(format!("weather provider JSON: {e}")))?;

    let current = body.current.map(|c| CurrentWeather {
        time: c.time,
        weather_code: c.weather_code,
        summary: c.weather_code.map(weather_code_summary).map(str::to_string),
        temperature_c: c.temperature_2m,
        apparent_temperature_c: c.apparent_temperature,
        wind_speed_kmh: c.wind_speed_10m,
    });

    let days = body.daily.map(daily_from_open_meteo).unwrap_or_default();

    Ok(WeatherBrief {
        location,
        fetched_at: time::OffsetDateTime::now_utc(),
        provider: "Open-Meteo".to_string(),
        provider_url: OPEN_METEO_FORECAST_URL.to_string(),
        timezone: body.timezone,
        timezone_abbreviation: body.timezone_abbreviation,
        current,
        days,
    })
}

fn validate_location(loc: &WeatherLocation) -> Result<()> {
    if loc.label.trim().is_empty() {
        return Err(ApiError::BadRequest(
            "weather label must not be empty".into(),
        ));
    }
    if !(-90.0..=90.0).contains(&loc.latitude) {
        return Err(ApiError::BadRequest(
            "latitude must be between -90 and 90".into(),
        ));
    }
    if !(-180.0..=180.0).contains(&loc.longitude) {
        return Err(ApiError::BadRequest(
            "longitude must be between -180 and 180".into(),
        ));
    }
    Ok(())
}

fn daily_from_open_meteo(daily: OpenMeteoDaily) -> Vec<DailyWeather> {
    daily
        .time
        .iter()
        .enumerate()
        .map(|(i, date)| {
            let code = nth(&daily.weather_code, i);
            DailyWeather {
                date: date.clone(),
                weather_code: code,
                summary: code.map(weather_code_summary).map(str::to_string),
                temp_min_c: nth(&daily.temperature_2m_min, i),
                temp_max_c: nth(&daily.temperature_2m_max, i),
                precipitation_probability_pct: nth(&daily.precipitation_probability_max, i),
                precipitation_sum_mm: nth(&daily.precipitation_sum, i),
            }
        })
        .collect()
}

fn nth<T: Copy>(values: &Option<Vec<Option<T>>>, idx: usize) -> Option<T> {
    values.as_ref().and_then(|xs| xs.get(idx)).and_then(|x| *x)
}

pub fn weather_code_summary(code: i32) -> &'static str {
    match code {
        0 => "Clear sky",
        1 => "Mainly clear",
        2 => "Partly cloudy",
        3 => "Overcast",
        45 | 48 => "Fog",
        51..=55 => "Drizzle",
        56 | 57 => "Freezing drizzle",
        61..=65 => "Rain",
        66 | 67 => "Freezing rain",
        71..=75 => "Snow",
        77 => "Snow grains",
        80..=82 => "Rain showers",
        85 | 86 => "Snow showers",
        95 => "Thunderstorm",
        96 | 99 => "Thunderstorm with hail",
        _ => "Unknown",
    }
}
