//! Google Calendar API v3 client — thin wrapper around
//! `reqwest::blocking` for the endpoints we need.
//!
//! v0.1 shape — implements the type shapes + the list-events
//! call. Insert / update / delete are stubbed.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

const API_BASE: &str = "https://www.googleapis.com/calendar/v3";

#[derive(Debug, Clone)]
pub struct Client {
    #[allow(dead_code)]
    http: reqwest::blocking::Client,
    access_token: String,
}

impl Client {
    pub fn new(access_token: impl Into<String>) -> Self {
        Self {
            http: reqwest::blocking::Client::new(),
            access_token: access_token.into(),
        }
    }

    /// GET /calendars/{calendarId}/events?timeMin=…&timeMax=…
    pub fn list_events(
        &self,
        calendar_id: &str,
        time_min: DateTime<Utc>,
        time_max: DateTime<Utc>,
    ) -> Result<Vec<Event>> {
        let url = format!(
            "{API_BASE}/calendars/{calendar_id}/events?timeMin={}&timeMax={}&singleEvents=true&orderBy=startTime",
            time_min.to_rfc3339(),
            time_max.to_rfc3339(),
        );
        let resp = self
            .http
            .get(&url)
            .bearer_auth(&self.access_token)
            .send()
            .with_context(|| format!("GET {url}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            anyhow::bail!("{status}: {body}");
        }
        let list: EventList = resp.json().context("parse events list")?;
        Ok(list.items)
    }
}

#[derive(Debug, Clone, Deserialize)]
struct EventList {
    #[serde(default)]
    items: Vec<Event>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub location: Option<String>,
    pub start: EventTime,
    pub end: EventTime,
    #[serde(default)]
    pub attendees: Vec<Attendee>,
    #[serde(default, rename = "hangoutLink")]
    pub hangout_link: Option<String>,
    #[serde(default, rename = "htmlLink")]
    pub html_link: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventTime {
    #[serde(default, rename = "dateTime")]
    pub date_time: Option<String>,
    #[serde(default)]
    pub date: Option<String>,
    #[serde(default, rename = "timeZone")]
    pub time_zone: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attendee {
    #[serde(default)]
    pub email: String,
    #[serde(default, rename = "displayName")]
    pub display_name: Option<String>,
    #[serde(default, rename = "responseStatus")]
    pub response_status: Option<String>,
    #[serde(default)]
    pub optional: bool,
    #[serde(default)]
    pub organizer: bool,
}
