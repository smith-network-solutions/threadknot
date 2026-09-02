//! ElevenLabs API client. The only place Threadknot talks to ElevenLabs.
//!
//! Everything here runs server-side: the webview's CSP has no external
//! origins, and the API key must never reach the frontend. Auth is the
//! `xi-api-key` header on every request. Responses are reshaped into the
//! camelCase wire forms the settings UI consumes, so the raw ElevenLabs
//! payloads never leak into the protocol.

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::time::Duration;

const BASE_URL: &str = "https://api.elevenlabs.io";
/// Ceiling on a JSON response body; the voices list is the largest and pages.
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

/// Same shape as `hermes::http_client`: connect timeout here, per-request
/// timeouts layered at each call site.
fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(8))
        .build()
        .expect("reqwest client")
}

/// Translate an ElevenLabs error response into something a person can act on.
/// The API nests detail as `{"detail": {"status": "...", "message": "..."}}`.
fn api_error(status: reqwest::StatusCode, body: &str) -> anyhow::Error {
    let detail_status = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.pointer("/detail/status")?.as_str().map(str::to_string));
    let message = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            let d = v.get("detail")?;
            d.as_str()
                .map(str::to_string)
                .or_else(|| d.pointer("/message")?.as_str().map(str::to_string))
        })
        .unwrap_or_else(|| body.chars().take(200).collect());
    match (status.as_u16(), detail_status.as_deref()) {
        (401, _) | (_, Some("invalid_api_key")) => {
            anyhow!("ElevenLabs rejected the API key — open Settings → Voice to reconnect")
        }
        (_, Some("quota_exceeded")) => {
            anyhow!("ElevenLabs credits are exhausted for this billing cycle")
        }
        (429, _) | (_, Some("too_many_concurrent_requests")) => {
            anyhow!("ElevenLabs is rate-limiting — try again in a moment")
        }
        _ => anyhow!("ElevenLabs returned {status}: {message}"),
    }
}

async fn get_json(api_key: &str, path: &str, query: &[(&str, String)]) -> Result<Value> {
    let response = http_client()
        .get(format!("{BASE_URL}{path}"))
        .header("xi-api-key", api_key)
        .query(query)
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .context("could not reach ElevenLabs")?;
    let status = response.status();
    let body = response
        .bytes()
        .await
        .context("could not read the ElevenLabs response")?;
    anyhow::ensure!(
        body.len() <= MAX_RESPONSE_BYTES,
        "ElevenLabs response was too large"
    );
    let body = String::from_utf8_lossy(&body);
    if !status.is_success() {
        return Err(api_error(status, &body));
    }
    serde_json::from_str(&body).context("ElevenLabs returned invalid JSON")
}

/// `GET /v1/user/subscription` — live account state. This is also the key
/// validation call: a bad key fails here with the auth error.
pub async fn subscription(api_key: &str) -> Result<Value> {
    let v = get_json(api_key, "/v1/user/subscription", &[]).await?;
    Ok(shape_subscription(&v))
}

/// Reshape the subscription response for the wire. Live values only; anything
/// the API does not expose stays absent rather than assumed.
pub fn shape_subscription(v: &Value) -> Value {
    let reset = v["next_character_count_reset_unix"]
        .as_i64()
        .and_then(|secs| chrono::DateTime::from_timestamp(secs, 0))
        .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
    json!({
        "tier": v["tier"].as_str(),
        "characterCount": v["character_count"].as_u64(),
        "characterLimit": v["character_limit"].as_u64(),
        "nextResetAt": reset,
        "status": v["status"].as_str(),
    })
}

/// `GET /v1/models`, filtered to TTS-capable models and reshaped with the
/// capability flags the settings UI uses to enable/disable tuning controls.
pub async fn models(api_key: &str) -> Result<Value> {
    let v = get_json(api_key, "/v1/models", &[]).await?;
    Ok(json!({ "models": shape_models(&v) }))
}

pub fn shape_models(v: &Value) -> Vec<Value> {
    let Some(list) = v.as_array() else {
        return Vec::new();
    };
    list.iter()
        .filter(|m| m["can_do_text_to_speech"].as_bool().unwrap_or(false))
        .map(|m| {
            let id = m["model_id"].as_str().unwrap_or_default();
            let languages: Vec<&str> = m["languages"]
                .as_array()
                .map(|ls| ls.iter().filter_map(|l| l["name"].as_str()).collect())
                .unwrap_or_default();
            json!({
                "id": id,
                "name": m["name"].as_str(),
                "description": m["description"].as_str(),
                "languages": languages,
                "supportsStyle": m["can_use_style"].as_bool().unwrap_or(false),
                "supportsSpeakerBoost": m["can_use_speaker_boost"].as_bool().unwrap_or(false),
                // The models endpoint has no speed flag; request-level speed is
                // accepted across current TTS models, so the control stays on.
                "supportsSpeed": true,
                "maxCharacters": m["maximum_text_length_per_request"].as_u64(),
                "costFactor": m["token_cost_factor"].as_f64(),
                // Flash/Turbo are ElevenLabs' low-latency families — what a
                // live conversation wants.
                "recommended": id.starts_with("eleven_flash") || id.starts_with("eleven_turbo"),
            })
        })
        .collect()
}

/// `GET /v2/voices` — search/filter/pagination over the account's voices.
pub async fn voices_search(
    api_key: &str,
    search: Option<&str>,
    category: Option<&str>,
    page_token: Option<&str>,
) -> Result<Value> {
    let mut query: Vec<(&str, String)> = vec![("page_size", "30".into())];
    if let Some(s) = search.filter(|s| !s.trim().is_empty()) {
        query.push(("search", s.trim().to_string()));
    }
    if let Some(c) = category.filter(|c| !c.trim().is_empty()) {
        query.push(("category", c.trim().to_string()));
    }
    if let Some(t) = page_token.filter(|t| !t.trim().is_empty()) {
        query.push(("next_page_token", t.trim().to_string()));
    }
    let v = get_json(api_key, "/v2/voices", &query).await?;
    let voices: Vec<Value> = v["voices"]
        .as_array()
        .map(|vs| vs.iter().map(shape_voice).collect())
        .unwrap_or_default();
    Ok(json!({
        "voices": voices,
        "nextPageToken": v["next_page_token"].as_str().filter(|_| v["has_more"].as_bool().unwrap_or(false)),
        "totalCount": v["total_count"].as_u64(),
    }))
}

pub fn shape_voice(v: &Value) -> Value {
    let labels = &v["labels"];
    json!({
        "id": v["voice_id"].as_str(),
        "name": v["name"].as_str(),
        "category": v["category"].as_str(),
        "description": v["description"].as_str(),
        "previewUrl": v["preview_url"].as_str(),
        "labels": {
            "accent": labels["accent"].as_str(),
            "gender": labels["gender"].as_str(),
            "age": labels["age"].as_str(),
            "useCase": labels["use_case"].as_str(),
            "language": labels["language"].as_str(),
            "descriptive": labels["descriptive"].as_str(),
        },
    })
}

/// `GET /v1/voices/{id}` — one voice, for validating a selection still exists.
pub async fn voice(api_key: &str, voice_id: &str) -> Result<Value> {
    let v = get_json(api_key, &format!("/v1/voices/{voice_id}"), &[]).await?;
    Ok(shape_voice(&v))
}

/// `GET /v1/voices/settings/default` — ElevenLabs' own defaults, for the
/// settings screen's reset button (live values, not hard-coded assumptions).
pub async fn default_voice_settings(api_key: &str) -> Result<Value> {
    let v = get_json(api_key, "/v1/voices/settings/default", &[]).await?;
    Ok(shape_voice_settings(&v))
}

pub fn shape_voice_settings(v: &Value) -> Value {
    json!({
        "stability": v["stability"].as_f64(),
        "similarityBoost": v["similarity_boost"].as_f64(),
        "style": v["style"].as_f64(),
        "useSpeakerBoost": v["use_speaker_boost"].as_bool(),
        "speed": v["speed"].as_f64(),
    })
}

/// The `voice_settings` block for a TTS request, from the stored tuning.
/// Absent knobs are omitted so the voice's own defaults apply.
pub fn tts_voice_settings(tuning: &super::VoiceTuning) -> Option<Value> {
    let mut settings = serde_json::Map::new();
    if let Some(v) = tuning.stability {
        settings.insert("stability".into(), json!(v));
    }
    if let Some(v) = tuning.similarity_boost {
        settings.insert("similarity_boost".into(), json!(v));
    }
    if let Some(v) = tuning.style {
        settings.insert("style".into(), json!(v));
    }
    if let Some(v) = tuning.use_speaker_boost {
        settings.insert("use_speaker_boost".into(), json!(v));
    }
    if let Some(v) = tuning.speed {
        settings.insert("speed".into(), json!(v));
    }
    (!settings.is_empty()).then(|| Value::Object(settings))
}

/// `POST /v1/text-to-speech/{voice}/stream` — the streaming TTS request.
/// Returns the live response; the caller consumes `bytes_stream()` so audio
/// can start playing before generation finishes.
pub async fn tts_stream(
    api_key: &str,
    voice_id: &str,
    model_id: &str,
    output_format: &str,
    text: &str,
    tuning: &super::VoiceTuning,
) -> Result<reqwest::Response> {
    let mut body = json!({
        "text": text,
        "model_id": model_id,
    });
    if let Some(settings) = tts_voice_settings(tuning) {
        body["voice_settings"] = settings;
    }
    let response = http_client()
        .post(format!("{BASE_URL}/v1/text-to-speech/{voice_id}/stream"))
        .header("xi-api-key", api_key)
        .query(&[("output_format", output_format)])
        .json(&body)
        // Generous: covers generation of a long paragraph; the stream itself
        // starts long before this.
        .timeout(Duration::from_secs(120))
        .send()
        .await
        .context("could not reach ElevenLabs for speech")?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(api_error(status, &body));
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscription_shape_uses_live_values_only() {
        let raw = json!({
            "tier": "pro",
            "character_count": 142_300,
            "character_limit": 600_000,
            "next_character_count_reset_unix": 1_760_000_000,
            "status": "active",
        });
        let shaped = shape_subscription(&raw);
        assert_eq!(shaped["tier"], "pro");
        assert_eq!(shaped["characterCount"], 142_300);
        assert_eq!(shaped["characterLimit"], 600_000);
        assert!(shaped["nextResetAt"].as_str().unwrap().starts_with("2025"));
        // Absent fields stay null rather than being invented.
        let empty = shape_subscription(&json!({}));
        assert!(empty["tier"].is_null());
        assert!(empty["nextResetAt"].is_null());
    }

    #[test]
    fn models_filter_to_tts_and_flag_low_latency() {
        let raw = json!([
            {
                "model_id": "eleven_flash_v2_5",
                "name": "Flash v2.5",
                "can_do_text_to_speech": true,
                "can_use_style": false,
                "can_use_speaker_boost": true,
                "languages": [{ "language_id": "en", "name": "English" }],
            },
            {
                "model_id": "eleven_multilingual_v2",
                "name": "Multilingual v2",
                "can_do_text_to_speech": true,
                "can_use_style": true,
                "can_use_speaker_boost": true,
            },
            { "model_id": "scribe_v1", "name": "Scribe", "can_do_text_to_speech": false },
        ]);
        let models = shape_models(&raw);
        assert_eq!(models.len(), 2);
        assert_eq!(models[0]["id"], "eleven_flash_v2_5");
        assert_eq!(models[0]["recommended"], true);
        assert_eq!(models[0]["supportsStyle"], false);
        assert_eq!(models[1]["recommended"], false);
        assert_eq!(models[1]["supportsStyle"], true);
    }

    #[test]
    fn error_mapping_is_actionable() {
        let auth = api_error(reqwest::StatusCode::UNAUTHORIZED, "{}");
        assert!(auth.to_string().contains("rejected the API key"));
        let quota = api_error(
            reqwest::StatusCode::PAYMENT_REQUIRED,
            r#"{"detail":{"status":"quota_exceeded","message":"x"}}"#,
        );
        assert!(quota.to_string().contains("credits are exhausted"));
        let rate = api_error(reqwest::StatusCode::TOO_MANY_REQUESTS, "{}");
        assert!(rate.to_string().contains("rate-limiting"));
    }

    #[test]
    fn tuning_omits_unset_knobs() {
        assert!(tts_voice_settings(&super::super::VoiceTuning::default()).is_none());
        let some = tts_voice_settings(&super::super::VoiceTuning {
            stability: Some(0.5),
            speed: Some(1.1),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(some["stability"], 0.5);
        assert_eq!(some["speed"], 1.1);
        assert!(some.get("style").is_none());
    }
}
