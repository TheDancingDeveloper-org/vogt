use std::sync::Arc;

use axum::{
    extract::{Query, State},
    Json,
};
use serde::Serialize;

use crate::{
    activity::ActivityState,
    app::AppState,
    error::Result,
    pty::SessionSummary,
    weather::{self, WeatherBrief, WeatherQuery},
};

#[derive(Debug, Serialize)]
pub struct DailyBriefing {
    #[serde(with = "time::serde::rfc3339")]
    pub generated_at: time::OffsetDateTime,
    pub weather: Option<WeatherBrief>,
    pub sessions: SessionBriefing,
}

#[derive(Debug, Serialize)]
pub struct SessionBriefing {
    pub total: usize,
    pub running: usize,
    pub idle: usize,
    pub waiting_for_input: usize,
    pub errored: usize,
    pub waiting_sessions: Vec<SessionSummary>,
    pub errored_sessions: Vec<SessionSummary>,
}

pub async fn daily(
    State(state): State<Arc<AppState>>,
    Query(query): Query<WeatherQuery>,
) -> Result<Json<DailyBriefing>> {
    let weather = match weather::resolve_location(&query, state.config.weather_location.as_ref()) {
        Ok(location) => Some(weather::fetch_forecast(location).await?),
        Err(crate::ApiError::BadRequest(_))
            if query.latitude.is_none()
                && query.longitude.is_none()
                && state.config.weather_location.is_none() =>
        {
            None
        }
        Err(e) => return Err(e),
    };

    let sessions = summarize_sessions(state.sessions.list());

    Ok(Json(DailyBriefing {
        generated_at: time::OffsetDateTime::now_utc(),
        weather,
        sessions,
    }))
}

fn summarize_sessions(sessions: Vec<SessionSummary>) -> SessionBriefing {
    let total = sessions.len();
    let running = sessions
        .iter()
        .filter(|s| s.activity == ActivityState::Running)
        .count();
    let idle = sessions
        .iter()
        .filter(|s| s.activity == ActivityState::Idle)
        .count();
    let waiting_for_input = sessions
        .iter()
        .filter(|s| s.activity == ActivityState::WaitingForInput)
        .count();
    let errored = sessions
        .iter()
        .filter(|s| s.activity == ActivityState::Errored)
        .count();
    let waiting_sessions = sessions
        .iter()
        .filter(|s| s.activity == ActivityState::WaitingForInput)
        .cloned()
        .collect();
    let errored_sessions = sessions
        .into_iter()
        .filter(|s| s.activity == ActivityState::Errored)
        .collect();

    SessionBriefing {
        total,
        running,
        idle,
        waiting_for_input,
        errored,
        waiting_sessions,
        errored_sessions,
    }
}
