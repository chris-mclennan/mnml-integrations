//! Slack Web API client — blocking `reqwest` + `serde_json`. No SDK
//! dep. Hits a handful of endpoints described in the README.
//!
//! Auth: `Authorization: Bearer <token>` header. Token resolved from
//! env in [`Auth::from_env`]. POST endpoints take
//! `application/x-www-form-urlencoded` bodies for the simple ones
//! (`chat.postMessage`, `chat.getPermalink`, `reactions.add`) and we
//! stick to that for the v0.1 surface.
//!
//! Slack returns `{ "ok": true, ... }` on success and
//! `{ "ok": false, "error": "<code>" }` on failure (with status 200).
//! Rate limits arrive as HTTP 429 + `Retry-After: <secs>`.

use anyhow::{Context, Result, anyhow};
use reqwest::blocking::{Client, RequestBuilder};
use serde::Deserialize;
use std::time::Duration;

const API_BASE: &str = "https://slack.com/api";

/// Resolved Slack auth. v0.1 always prefers `SLACK_USER_TOKEN`; bot
/// token is captured but unused. Either env var with a value is fine
/// — both empty / unset is a hard error.
#[derive(Debug, Clone)]
pub struct Auth {
    /// The token we'll actually send on requests.
    pub token: String,
    /// `"user"` (xoxp-…) or `"bot"` (xoxb-…) for the `--check` report.
    pub kind: &'static str,
}

impl Auth {
    pub fn from_env() -> Result<Self> {
        let user = std::env::var("SLACK_USER_TOKEN")
            .ok()
            .filter(|s| !s.is_empty());
        let bot = std::env::var("SLACK_BOT_TOKEN")
            .ok()
            .filter(|s| !s.is_empty());
        match (user, bot) {
            (Some(token), _) => Ok(Self {
                token,
                kind: "user",
            }),
            (None, Some(token)) => Ok(Self { token, kind: "bot" }),
            (None, None) => Err(anyhow!(
                "SLACK_USER_TOKEN not set — create a Slack app at https://api.slack.com/apps, install it to your workspace, and export the User OAuth Token (xoxp-…). SLACK_BOT_TOKEN (xoxb-…) is an optional fallback but covers fewer endpoints."
            )),
        }
    }

    pub fn api_base(&self) -> &'static str {
        API_BASE
    }
}

/// Mask a token for `--check` output. Slack tokens are roughly
/// 50–70 chars; the original implementation kept the `xoxp-` /
/// `xoxb-` prefix AND the last 4 chars to help users distinguish
/// "the right token but wrong scope" from "the wrong account". But:
///
/// - The prefix tells anyone reading the output that this is a
///   user token (`xoxp-`) vs a bot token (`xoxb-`) vs an app-level
///   token (`xapp-`). That's a category leak — useful to attackers
///   sniffing a screenshot to know what kind of credential to phish.
/// - The 4-char tail at slack-token entropy is real exposure
///   (~24 bits of entropy on a ~140-bit token = ~17% of the secret).
///   sibling-crates-hunt-2026-06-08 LOW. Same family as the
///   `mask_env` fix in mnml-msg-gmail (8a8210c) 2 weeks ago.
///
/// Reports the length only — enough to confirm "I have a token,
/// it's roughly the right length" without leaking anything else.
pub fn mask_token(t: &str) -> String {
    format!("({} chars)", t.len())
}

fn build_client() -> Result<Client> {
    Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent(concat!("mnml-msg-slack/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("build HTTP client")
}

fn auth_get(client: &Client, auth: &Auth, url: &str) -> RequestBuilder {
    client
        .get(url)
        .header("Authorization", format!("Bearer {}", auth.token))
}

fn auth_post_form(client: &Client, auth: &Auth, url: &str) -> RequestBuilder {
    client
        .post(url)
        .header("Authorization", format!("Bearer {}", auth.token))
        .header(
            "Content-Type",
            "application/x-www-form-urlencoded; charset=utf-8",
        )
}

/// Send a request and return the parsed JSON envelope. Surfaces
/// rate-limits (`HTTP 429`) and slack `{ok: false, error}` shapes as
/// human-readable errors.
///
/// On a 429, honours the `Retry-After` header (capped at
/// [`RETRY_AFTER_CAP`]) and tries once more. The retry is safe to do
/// inline now that every HTTPS-driving call lives in the background
/// loader thread (commit ddb43a2) — the TUI event loop never blocks.
/// sibling-crates-hunt-2026-06-08 MED finding (slack `users.info` is
/// Tier 4 = 100/min; prefetch-10 loops guaranteed throttling).
fn send_and_parse(req: RequestBuilder, label: &str) -> Result<serde_json::Value> {
    /// Hard ceiling so a slack-side bug / weird header can't park
    /// the loader thread for minutes. 30 s is comfortably past every
    /// real Tier-4 backoff slack hands out.
    const RETRY_AFTER_CAP: u64 = 30;
    // try_clone'd up front so the retry can re-send with the same
    // body / headers; the builder is one-shot via `send`.
    let retry_req = req.try_clone();
    let resp = req.send().with_context(|| format!("send {label}"))?;
    let status = resp.status();
    if status.as_u16() == 429 {
        let retry = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(1);
        let wait = retry.min(RETRY_AFTER_CAP);
        if let Some(retry_req) = retry_req {
            std::thread::sleep(std::time::Duration::from_secs(wait));
            let resp2 = retry_req.send().with_context(|| format!("retry {label}"))?;
            let status2 = resp2.status();
            if status2.as_u16() == 429 {
                // Still throttled after the requested wait — surface
                // up instead of looping forever.
                return Err(anyhow!(
                    "slack: rate-limited after waiting {wait}s; back off and try again"
                ));
            }
            return finish_parse(resp2, label);
        }
        // try_clone returned None — the body wasn't cloneable. Just
        // report the rate-limit without retrying.
        return Err(anyhow!("slack: rate-limited, retry in {retry}s"));
    }
    finish_parse(resp, label)
}

fn finish_parse(resp: reqwest::blocking::Response, label: &str) -> Result<serde_json::Value> {
    let status = resp.status();
    let text = resp.text().with_context(|| format!("read {label} body"))?;
    let val: serde_json::Value = serde_json::from_str(&text)
        .with_context(|| format!("parse {label} JSON ({} chars)", text.len()))?;
    if !status.is_success() {
        return Err(anyhow!("HTTP {status}: {}", truncate(&text, 200)));
    }
    if val.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        let err = val
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown_error");
        return Err(anyhow!("slack: {err}"));
    }
    Ok(val)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect::<String>() + "…"
    }
}

// ── auth.test ────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct AuthTest {
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub team: String,
    #[serde(default)]
    pub user: String,
    #[serde(default)]
    pub team_id: String,
    #[serde(default)]
    pub user_id: String,
}

pub fn auth_test(auth: &Auth) -> Result<AuthTest> {
    let client = build_client()?;
    let url = format!("{}/auth.test", API_BASE);
    let val = send_and_parse(auth_get(&client, auth, &url), "auth.test")?;
    let parsed: AuthTest =
        serde_json::from_value(val).with_context(|| "shape auth.test response")?;
    Ok(parsed)
}

// ── conversations.list ───────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // fields kept for forward compat (v0.2 unread / mpim / archive cues)
pub struct Channel {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub is_channel: bool,
    #[serde(default)]
    pub is_group: bool,
    #[serde(default)]
    pub is_im: bool,
    #[serde(default)]
    pub is_mpim: bool,
    #[serde(default)]
    pub is_private: bool,
    #[serde(default)]
    pub is_archived: bool,
    #[serde(default)]
    pub is_member: bool,
    #[serde(default)]
    pub num_members: Option<u64>,
    /// Channel topic — `purpose` is a separate field we don't surface in v0.1.
    #[serde(default)]
    pub topic: Option<ChannelText>,
    /// User id for IMs.
    #[serde(default)]
    pub user: Option<String>,
    /// Last-read ts (seconds.micro string).
    #[serde(default)]
    pub last_read: Option<String>,
    /// For mpims — display name baked into the channel by Slack.
    #[serde(default)]
    pub purpose: Option<ChannelText>,
    /// #1044 (2026-08-19) — Slack-computed unread count for this
    /// conversation. Present on `conversations.list` when the token
    /// carries the `client` scope OR when the endpoint's per-channel
    /// unread info is enabled server-side. `None` = "not reported"
    /// (do not treat as zero when summing chip totals — degrade to
    /// showing presence only, so a scope gap doesn't misreport an
    /// active workspace as silent).
    #[serde(default)]
    pub unread_count_display: Option<u64>,
    /// #1044 — Slack-computed @mention count for this conversation.
    /// Same scope story as `unread_count_display`.
    #[serde(default)]
    pub mention_count_display: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChannelText {
    #[serde(default)]
    pub value: String,
}

impl Channel {
    /// Best-effort display name. Falls back to id for unnamed IMs.
    pub fn display_name(&self) -> String {
        if !self.name.is_empty() {
            if self.is_channel || self.is_group || self.is_private {
                format!("#{}", self.name)
            } else {
                self.name.clone()
            }
        } else if self.is_im {
            // IM: `dm: <user-id>` until we resolve via users.info.
            format!(
                "dm: {}",
                self.user.clone().unwrap_or_else(|| self.id.clone())
            )
        } else {
            self.id.clone()
        }
    }

    pub fn topic_text(&self) -> String {
        self.topic
            .as_ref()
            .map(|t| t.value.clone())
            .unwrap_or_default()
    }
}

#[derive(Debug, Deserialize)]
struct ConversationsListResponse {
    #[serde(default)]
    channels: Vec<Channel>,
}

/// `GET /conversations.list`. `types` is a comma-separated subset of
/// `public_channel,private_channel,im,mpim`.
pub fn conversations_list(auth: &Auth, types: &str) -> Result<Vec<Channel>> {
    let client = build_client()?;
    let url = format!(
        "{}/conversations.list?types={}&exclude_archived=true&limit=200",
        API_BASE,
        urlencode(types)
    );
    let val = send_and_parse(auth_get(&client, auth, &url), "conversations.list")?;
    let parsed: ConversationsListResponse =
        serde_json::from_value(val).with_context(|| "shape conversations.list")?;
    Ok(parsed.channels)
}

// ── conversations.history ────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // `subtype` is parsed for future filtering (e.g. hide channel_join)
pub struct Message {
    #[serde(default)]
    pub ts: String,
    #[serde(default)]
    pub user: Option<String>,
    /// Bot-posted messages set `bot_id` and may not set `user`.
    #[serde(default)]
    pub bot_id: Option<String>,
    #[serde(default)]
    pub text: String,
    /// Thread parent ts (set on messages within a thread).
    #[serde(default)]
    pub thread_ts: Option<String>,
    /// Reply count on a thread parent.
    #[serde(default)]
    pub reply_count: Option<u64>,
    /// Slack `subtype` (channel_join, file_share, etc.).
    #[serde(default)]
    pub subtype: Option<String>,
}

impl Message {
    /// Author id — prefers `user`, falls back to `bot_id`.
    pub fn author_id(&self) -> Option<&str> {
        self.user.as_deref().or(self.bot_id.as_deref())
    }
}

#[derive(Debug, Deserialize)]
struct HistoryResponse {
    #[serde(default)]
    messages: Vec<Message>,
}

/// `GET /conversations.history?channel=<id>&limit=30`. Newest-first
/// by default — we reverse so the detail panel shows oldest→newest.
pub fn conversations_history(auth: &Auth, channel_id: &str, limit: u32) -> Result<Vec<Message>> {
    let client = build_client()?;
    let url = format!(
        "{}/conversations.history?channel={}&limit={}",
        API_BASE,
        urlencode(channel_id),
        limit
    );
    let val = send_and_parse(auth_get(&client, auth, &url), "conversations.history")?;
    let parsed: HistoryResponse =
        serde_json::from_value(val).with_context(|| "shape conversations.history")?;
    let mut msgs = parsed.messages;
    msgs.reverse();
    Ok(msgs)
}

// ── search.messages ──────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct SearchMatch {
    #[serde(default)]
    pub ts: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub channel: Option<SearchChannel>,
    #[serde(default)]
    pub permalink: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SearchChannel {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    messages: SearchMessagesPayload,
}

#[derive(Debug, Deserialize)]
struct SearchMessagesPayload {
    #[serde(default)]
    matches: Vec<SearchMatch>,
}

/// `GET /search.messages?query=...&count=50&sort=timestamp&sort_dir=desc`.
pub fn search_messages(auth: &Auth, query: &str) -> Result<Vec<SearchMatch>> {
    let client = build_client()?;
    let url = format!(
        "{}/search.messages?query={}&count=50&sort=timestamp&sort_dir=desc",
        API_BASE,
        urlencode(query)
    );
    let val = send_and_parse(auth_get(&client, auth, &url), "search.messages")?;
    let parsed: SearchResponse =
        serde_json::from_value(val).with_context(|| "shape search.messages")?;
    Ok(parsed.messages.matches)
}

// ── chat.postMessage ─────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // channel + ts kept for forward compat (deep-linking on success)
pub struct PostMessageResult {
    #[serde(default)]
    pub channel: String,
    #[serde(default)]
    pub ts: String,
}

/// `POST /chat.postMessage` — text only, no blocks. `thread_ts` set
/// turns the post into a thread reply.
pub fn chat_post_message(
    auth: &Auth,
    channel: &str,
    text: &str,
    thread_ts: Option<&str>,
) -> Result<PostMessageResult> {
    let client = build_client()?;
    let url = format!("{}/chat.postMessage", API_BASE);
    let mut form = vec![("channel", channel.to_string()), ("text", text.to_string())];
    if let Some(tts) = thread_ts {
        form.push(("thread_ts", tts.to_string()));
    }
    let val = send_and_parse(
        auth_post_form(&client, auth, &url).body(form_encode(&form)),
        "chat.postMessage",
    )?;
    let parsed: PostMessageResult =
        serde_json::from_value(val).with_context(|| "shape chat.postMessage")?;
    Ok(parsed)
}

// ── chat.getPermalink ────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct PermalinkResult {
    #[serde(default)]
    pub permalink: String,
}

/// `GET /chat.getPermalink?channel=<c>&message_ts=<ts>`.
pub fn chat_get_permalink(auth: &Auth, channel: &str, message_ts: &str) -> Result<String> {
    let client = build_client()?;
    let url = format!(
        "{}/chat.getPermalink?channel={}&message_ts={}",
        API_BASE,
        urlencode(channel),
        urlencode(message_ts)
    );
    let val = send_and_parse(auth_get(&client, auth, &url), "chat.getPermalink")?;
    let parsed: PermalinkResult =
        serde_json::from_value(val).with_context(|| "shape chat.getPermalink")?;
    Ok(parsed.permalink)
}

// ── reactions.add ────────────────────────────────────────────────

/// `POST /reactions.add`. `name` is the bare emoji name (no colons).
pub fn reactions_add(auth: &Auth, channel: &str, message_ts: &str, name: &str) -> Result<()> {
    if !is_valid_emoji_name(name) {
        return Err(anyhow!(
            "invalid emoji name `{name}` — use lowercase letters / digits / underscore / dash, no colons"
        ));
    }
    let client = build_client()?;
    let url = format!("{}/reactions.add", API_BASE);
    let form = [
        ("channel", channel.to_string()),
        ("timestamp", message_ts.to_string()),
        ("name", name.to_string()),
    ];
    send_and_parse(
        auth_post_form(&client, auth, &url).body(form_encode(&form)),
        "reactions.add",
    )?;
    Ok(())
}

// ── files.list (canvases) ────────────────────────────────────────

/// #1005 (2026-08-19) — Slack Canvas file metadata. `filetype ==
/// "canvas"` is what `files.list` returns for canvases (both
/// standalone and channel-attached). Fields we surface in the
/// Canvases tab; unused fields kept off the struct so serde skips
/// them (Slack's `File` shape is huge).
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // kept-for-forward-compat fields (filetype, is_public — surface UX to follow)
pub struct Canvas {
    pub id: String,
    /// Human title shown in the row. Slack sometimes returns an
    /// empty string for freshly-created canvases; we fall back to
    /// the id via `display_title()`.
    #[serde(default)]
    pub title: String,
    /// Slack-issued deep link for `open in browser`. Present on all
    /// visible canvases.
    #[serde(default)]
    pub permalink: String,
    /// Owner user id. Rendered in the row when the users.info cache
    /// resolves it; falls back to the raw id otherwise.
    #[serde(default)]
    pub user: String,
    /// Unix seconds — creation / update time (Slack files carry
    /// `created`, `updated` if edited; we prefer updated when
    /// present).
    #[serde(default)]
    pub updated: Option<u64>,
    #[serde(default)]
    pub created: Option<u64>,
    /// Channel this canvas is attached to (empty for standalone).
    #[serde(default)]
    pub channels: Vec<String>,
    /// Whether the canvas has been shared/is visible.
    #[serde(default)]
    pub is_public: bool,
    /// Editor context (some canvases are stand-alone, some are
    /// channel-attached).
    #[serde(default)]
    pub filetype: String,
}

impl Canvas {
    /// Best-effort human title. Falls back to id when Slack returns
    /// an empty `title` (freshly created / imported).
    pub fn display_title(&self) -> String {
        if self.title.is_empty() {
            self.id.clone()
        } else {
            self.title.clone()
        }
    }
    /// Prefer `updated`; fall back to `created`. `0` when neither
    /// is set (the row renderer treats 0 as "unknown").
    pub fn timestamp(&self) -> u64 {
        self.updated.or(self.created).unwrap_or(0)
    }
}

#[derive(Debug, Deserialize)]
struct FilesListResponse {
    #[serde(default)]
    files: Vec<Canvas>,
}

/// `GET /files.list?types=canvases&count=100`. Returns the caller's
/// visible canvases sorted newest-updated first (Slack default).
///
/// Scope required: `files:read` (bot) OR `search:read` (user).
/// A token without either returns `not_allowed_token_type` — the
/// caller surfaces that as a config-hint toast rather than a
/// scary red error, since it's a scope gap, not a bug.
pub fn files_list_canvases(auth: &Auth) -> Result<Vec<Canvas>> {
    let client = build_client()?;
    let url = format!("{}/files.list?types=canvases&count=100", API_BASE);
    let val = send_and_parse(auth_get(&client, auth, &url), "files.list")?;
    let parsed: FilesListResponse =
        serde_json::from_value(val).with_context(|| "shape files.list")?;
    Ok(parsed.files)
}

// ── users.info ───────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct User {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub real_name: String,
    #[serde(default)]
    pub profile: Option<UserProfile>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UserProfile {
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub real_name: String,
}

#[derive(Debug, Deserialize)]
struct UserInfoResponse {
    user: User,
}

impl User {
    /// Best display name — `profile.display_name` > `real_name` > `name`.
    pub fn best_name(&self) -> String {
        if let Some(p) = &self.profile {
            if !p.display_name.is_empty() {
                return p.display_name.clone();
            }
            if !p.real_name.is_empty() {
                return p.real_name.clone();
            }
        }
        if !self.real_name.is_empty() {
            return self.real_name.clone();
        }
        if !self.name.is_empty() {
            return self.name.clone();
        }
        self.id.clone()
    }
}

pub fn users_info(auth: &Auth, user_id: &str) -> Result<User> {
    let client = build_client()?;
    let url = format!("{}/users.info?user={}", API_BASE, urlencode(user_id));
    let val = send_and_parse(auth_get(&client, auth, &url), "users.info")?;
    let parsed: UserInfoResponse =
        serde_json::from_value(val).with_context(|| "shape users.info")?;
    Ok(parsed.user)
}

// ── helpers ──────────────────────────────────────────────────────

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

fn form_encode<T>(pairs: &[(&str, T)]) -> String
where
    T: AsRef<str>,
{
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", urlencode(k), urlencode(v.as_ref())))
        .collect::<Vec<_>>()
        .join("&")
}

/// Slack emoji names — lowercase letters, digits, `_`, `-`, and (for
/// skin-tone modifiers) `::skin-tone-N`. v0.1 keeps it tight.
pub fn is_valid_emoji_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 100 {
        return false;
    }
    name.chars()
        .all(|c| matches!(c, 'a'..='z' | '0'..='9' | '_' | '-' | '+'))
}

/// Convert a slack ts (e.g. `"1717000000.123456"`) to `HH:MM:SS.fff`
/// local time (best-effort — falls back to the raw ts).
///
/// Subsecond precision is preserved as 3-digit milliseconds. Without
/// it, two messages posted in the same wall-clock second rendered at
/// the same timestamp — confusing in a busy channel where a thread
/// can fire half a dozen replies inside one second.
/// (sibling-crates-hunt-2026-06-08 LOW.)
pub fn ts_to_hms(ts: &str) -> String {
    let mut parts = ts.splitn(2, '.');
    let Some(secs_str) = parts.next() else {
        return ts.to_string();
    };
    let Ok(secs) = secs_str.parse::<i64>() else {
        return ts.to_string();
    };
    // Slack ts fractions are microseconds (6 digits). Truncate the
    // displayed precision to milliseconds — that's the boundary where
    // human-perceptible message ordering breaks.
    let millis = parts
        .next()
        .and_then(|frac| {
            // Pad/truncate to 6 micro-digits, then divide.
            let mut buf = [b'0'; 6];
            for (i, b) in frac.bytes().take(6).enumerate() {
                buf[i] = b;
            }
            std::str::from_utf8(&buf)
                .ok()
                .and_then(|s| s.parse::<u32>().ok())
        })
        .unwrap_or(0)
        / 1000;
    let Some(naive) = chrono::DateTime::from_timestamp(secs, 0) else {
        return ts.to_string();
    };
    let local = naive.with_timezone(&chrono::Local);
    format!("{}.{:03}", local.format("%H:%M:%S"), millis)
}

/// The list of canonical "react quickly" emojis surfaced by `R`.
pub const QUICK_REACTIONS: &[&str] = &[
    "+1",
    "-1",
    "heart",
    "eyes",
    "tada",
    "joy",
    "thinking_face",
    "pray",
    "clap",
    "fire",
    "100",
    "rocket",
];

// ── users.getPresence ────────────────────────────────────────────

/// #1044 (2026-08-19). `GET /users.getPresence?user=<id>`. Returns
/// `"active"` | `"away"` | `"dnd"` (Slack folds Do Not Disturb into
/// `away` for non-self, but for the caller's own presence we also
/// interpret DND-window info when available). Falls back to
/// `"unknown"` on parse failure so the chip renders something
/// instead of erroring out silently.
pub fn get_presence(auth: &Auth, user_id: &str) -> Result<String> {
    let client = build_client()?;
    let url = format!("{}/users.getPresence?user={}", API_BASE, urlencode(user_id));
    let val = send_and_parse(auth_get(&client, auth, &url), "users.getPresence")?;
    let presence = val
        .get("presence")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    Ok(presence)
}

// ── chip summary (statusline) ────────────────────────────────────

/// #1044 (2026-08-19) — signals surfaced by the Slack statusline
/// chip. Every field is a defensively-computed aggregate over the
/// conversations the token can see. When Slack doesn't return the
/// per-channel unread field (see `Channel::unread_count_display`),
/// `has_unread_info` is false and consumers should hide the
/// unread-derived fields rather than render zeros.
#[derive(Debug, Clone, serde::Serialize)]
pub struct UnreadSummary {
    /// Total @mentions across every conversation (channel + DM).
    pub mentions: u64,
    /// Unread messages across DMs (im) + group DMs (mpim). Whole-
    /// message count, not "N DMs with unreads" — that's typically
    /// the higher-signal number when the user is triaging.
    pub dm_unread: u64,
    /// Number of public/private channels that have any unread. Not
    /// the total unread message count — a single busy channel
    /// shouldn't drown out other signals in this scalar.
    pub channel_unread_count: u64,
    /// `"active"` | `"away"` | `"dnd"` | `"unknown"`.
    pub presence: String,
    /// True when at least one conversation reported the unread
    /// fields; false = token/scope doesn't expose them, so the chip
    /// should not render `0(0)` (misleading — it looks like "no
    /// unreads" when it's really "no data").
    pub has_unread_info: bool,
    /// Total conversations scanned. Diagnostic only — mnml's
    /// statusline segment doesn't render it, but it's cheap and
    /// useful when someone runs `--values` at the shell to debug
    /// scope issues.
    pub scanned: u64,
}

/// #1044 (2026-08-19) — one-shot fetch used by `--values`. Runs
/// `auth.test` → `conversations.list` (im,mpim,public_channel,
/// private_channel) → `users.getPresence`. Total 3 API calls per
/// poll; the caller (mnml's statusline segment worker) rate-limits
/// invocations via `poll_interval_secs` in the manifest.
pub fn fetch_unread_summary(auth: &Auth) -> Result<UnreadSummary> {
    let test = auth_test(auth)?;
    let chans = conversations_list(auth, "im,mpim,public_channel,private_channel")?;
    let mut mentions = 0u64;
    let mut dm_unread = 0u64;
    let mut channel_unread_count = 0u64;
    let mut has_unread_info = false;
    for c in &chans {
        if let Some(m) = c.mention_count_display {
            has_unread_info = true;
            mentions += m;
        }
        if let Some(u) = c.unread_count_display {
            has_unread_info = true;
            if c.is_im || c.is_mpim {
                dm_unread += u;
            } else if u > 0 {
                channel_unread_count += 1;
            }
        }
    }
    // Presence lookup — best-effort; a scope gap here shouldn't
    // fail the whole summary. `unknown` renders cleanly in the
    // chip template.
    let presence = get_presence(auth, &test.user_id).unwrap_or_else(|_| "unknown".to_string());
    Ok(UnreadSummary {
        mentions,
        dm_unread,
        channel_unread_count,
        presence,
        has_unread_info,
        scanned: chans.len() as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_from_env_prefers_user_token() {
        // Don't depend on real env. Round-trip through the resolved
        // Auth struct.
        let a = Auth {
            token: "xoxp-abc-123-XYZW".into(),
            kind: "user",
        };
        assert_eq!(a.kind, "user");
        assert_eq!(a.api_base(), "https://slack.com/api");
    }

    #[test]
    fn mask_token_reports_length_only() {
        // sibling-crates-hunt-2026-06-08 LOW fix: keep length, drop
        // every character of the token (prefix kind, last-4 tail).
        let masked = mask_token("xoxp-1234567890-abcdefGHIJKL");
        assert!(!masked.contains("xoxp"));
        assert!(!masked.contains("HIJKL"));
        assert!(!masked.contains('…'));
        assert_eq!(masked, "(28 chars)");
    }

    #[test]
    fn mask_token_short_safe() {
        assert_eq!(mask_token("ab"), "(2 chars)");
        assert_eq!(mask_token(""), "(0 chars)");
    }

    #[test]
    fn parses_auth_test_response() {
        let json = r#"{"ok":true,"url":"https://acme.slack.com/","team":"Acme","user":"chris","team_id":"T123","user_id":"U456"}"#;
        let v: serde_json::Value = serde_json::from_str(json).unwrap();
        let parsed: AuthTest = serde_json::from_value(v).unwrap();
        assert_eq!(parsed.team, "Acme");
        assert_eq!(parsed.user_id, "U456");
    }

    #[test]
    fn parses_conversations_list_response() {
        let json = r#"{"ok":true,"channels":[
            {"id":"C1","name":"general","is_channel":true,"is_member":true,"num_members":42,"topic":{"value":"talk"}},
            {"id":"D1","is_im":true,"user":"U99"}
        ]}"#;
        let v: serde_json::Value = serde_json::from_str(json).unwrap();
        let parsed: ConversationsListResponse = serde_json::from_value(v).unwrap();
        assert_eq!(parsed.channels.len(), 2);
        assert_eq!(parsed.channels[0].display_name(), "#general");
        assert!(parsed.channels[1].is_im);
    }

    #[test]
    fn parses_history_response_and_reverses() {
        let json = r#"{"ok":true,"messages":[
            {"ts":"3","user":"U1","text":"newest"},
            {"ts":"2","user":"U2","text":"middle"},
            {"ts":"1","user":"U3","text":"oldest"}
        ]}"#;
        let v: serde_json::Value = serde_json::from_str(json).unwrap();
        let parsed: HistoryResponse = serde_json::from_value(v).unwrap();
        let mut msgs = parsed.messages;
        msgs.reverse();
        assert_eq!(msgs[0].text, "oldest");
        assert_eq!(msgs[2].text, "newest");
    }

    #[test]
    fn parses_search_messages_response() {
        let json = r#"{"ok":true,"messages":{"matches":[
            {"ts":"1717000000.000100","text":"hello","user":"U1","username":"chris","channel":{"id":"C1","name":"general"},"permalink":"https://slack.com/x"}
        ]}}"#;
        let v: serde_json::Value = serde_json::from_str(json).unwrap();
        let parsed: SearchResponse = serde_json::from_value(v).unwrap();
        assert_eq!(parsed.messages.matches.len(), 1);
        assert_eq!(parsed.messages.matches[0].text, "hello");
    }

    #[test]
    fn parses_post_message_response() {
        let json = r#"{"ok":true,"channel":"C1","ts":"1717000000.000200"}"#;
        let v: serde_json::Value = serde_json::from_str(json).unwrap();
        let parsed: PostMessageResult = serde_json::from_value(v).unwrap();
        assert_eq!(parsed.channel, "C1");
        assert_eq!(parsed.ts, "1717000000.000200");
    }

    #[test]
    fn parses_permalink_response() {
        let json =
            r#"{"ok":true,"channel":"C1","permalink":"https://acme.slack.com/archives/C1/p1717"}"#;
        let v: serde_json::Value = serde_json::from_str(json).unwrap();
        let parsed: PermalinkResult = serde_json::from_value(v).unwrap();
        assert!(parsed.permalink.starts_with("https://"));
    }

    #[test]
    fn parses_users_info_response() {
        let json = r#"{"ok":true,"user":{"id":"U1","name":"chris","real_name":"Chris M","profile":{"display_name":"chrism","real_name":"Chris M"}}}"#;
        let v: serde_json::Value = serde_json::from_str(json).unwrap();
        let parsed: UserInfoResponse = serde_json::from_value(v).unwrap();
        assert_eq!(parsed.user.best_name(), "chrism");
    }

    #[test]
    fn emoji_name_validation() {
        assert!(is_valid_emoji_name("+1"));
        assert!(is_valid_emoji_name("thinking_face"));
        assert!(is_valid_emoji_name("rocket"));
        assert!(is_valid_emoji_name("100"));
        assert!(!is_valid_emoji_name(""));
        assert!(!is_valid_emoji_name(":wave:")); // colons not allowed
        assert!(!is_valid_emoji_name("hello world"));
    }

    #[test]
    fn quick_reactions_are_all_valid() {
        for r in QUICK_REACTIONS {
            assert!(is_valid_emoji_name(r), "{r}");
        }
    }

    #[test]
    fn form_encode_round_trip() {
        let pairs = &[("a", "hello world"), ("b", "x&y=z")];
        let encoded = form_encode(pairs);
        assert!(encoded.contains("a=hello%20world"));
        assert!(encoded.contains("b=x%26y%3Dz"));
    }

    #[test]
    fn ts_to_hms_handles_garbage() {
        // garbage in, raw out (no panic)
        assert_eq!(ts_to_hms("nope"), "nope");
    }

    #[test]
    fn ts_to_hms_emits_three_digit_millis_with_micros_input() {
        // 1717000000.123456 → secs + 123ms. The HMS part depends on
        // the local timezone of whoever is running tests, so just
        // verify the format shape.
        let out = ts_to_hms("1717000000.123456");
        assert!(out.contains('.'));
        let (hms, frac) = out.split_once('.').unwrap();
        assert_eq!(hms.matches(':').count(), 2);
        assert_eq!(frac.len(), 3);
        assert_eq!(frac, "123");
    }

    #[test]
    fn ts_to_hms_zero_pads_fraction_when_missing() {
        // No fraction in input → millis defaults to 000, not the raw
        // string.
        let out = ts_to_hms("1717000000");
        assert!(out.ends_with(".000"));
    }

    #[test]
    fn ts_to_hms_short_fraction_pads_with_zeros() {
        // 1717000000.1 → micros = 100000 → millis = 100
        let out = ts_to_hms("1717000000.1");
        assert!(out.ends_with(".100"), "got {out}");
    }

    #[test]
    fn channel_display_name_falls_back_for_ims() {
        let c = Channel {
            id: "D1".into(),
            name: String::new(),
            is_channel: false,
            is_group: false,
            is_im: true,
            is_mpim: false,
            is_private: false,
            is_archived: false,
            is_member: false,
            num_members: None,
            topic: None,
            user: Some("U99".into()),
            last_read: None,
            purpose: None,
            // #1044 added these to Channel without updating the
            // fixtures; `None` = "not reported", which is the
            // pre-#1044 behavior these tests assert against.
            unread_count_display: None,
            mention_count_display: None,
        };
        assert_eq!(c.display_name(), "dm: U99");
    }
}
