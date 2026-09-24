//! Instance-owned speech service. Credentials stay on the server; harnesses only
//! receive the text the user chooses to send after dictation.
use std::io::Write;
use std::path::Path;
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, Multipart, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::state::AppState;

const SETTINGS_FILE: &str = "speech-settings.json";
const MAX_AUDIO_BYTES: usize = 25 * 1024 * 1024;
const MAX_SPEECH_BYTES: usize = 25 * 1024 * 1024;
const MAX_TRANSCRIPT_BYTES: usize = 1024 * 1024;
const MAX_SPEECH_CHARACTERS: usize = 4096;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/transcriptions", post(transcribe))
        .route(
            "/synthesize",
            post(synthesize).layer(DefaultBodyLimit::max(32 * 1024)),
        )
        .layer(DefaultBodyLimit::max(MAX_AUDIO_BYTES + 64 * 1024))
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(default)]
struct SpeechConfig {
    enabled: bool,
    base_url: String,
    api_key: String,
    stt_model: String,
    tts_model: String,
    voice: String,
}

impl Default for SpeechConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: "https://api.openai.com/v1".into(),
            api_key: String::new(),
            stt_model: "whisper-1".into(),
            tts_model: "tts-1".into(),
            voice: "alloy".into(),
        }
    }
}

#[derive(Serialize)]
pub(super) struct SpeechSettings {
    enabled: bool,
    base_url: String,
    has_api_key: bool,
    stt_model: String,
    tts_model: String,
    voice: String,
}

impl From<SpeechConfig> for SpeechSettings {
    fn from(config: SpeechConfig) -> Self {
        Self {
            enabled: config.enabled,
            has_api_key: !config.api_key.is_empty(),
            base_url: config.base_url,
            stt_model: config.stt_model,
            tts_model: config.tts_model,
            voice: config.voice,
        }
    }
}

#[derive(Deserialize)]
pub(super) struct UpdateSpeechSettings {
    enabled: bool,
    base_url: String,
    /// Omitted preserves the saved key; an empty string clears it.
    api_key: Option<String>,
    stt_model: String,
    tts_model: String,
    voice: String,
}

impl SpeechConfig {
    fn load(directory: &Path) -> Result<Self, SpeechError> {
        match std::fs::read(directory.join(SETTINGS_FILE)) {
            Ok(contents) => serde_json::from_slice(&contents)
                .map_err(|_| SpeechError::internal("Could not read speech settings")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(_) => Err(SpeechError::internal("Could not read speech settings")),
        }
    }

    fn save(&self, directory: &Path) -> Result<(), SpeechError> {
        // Match the existing collaboration credential store: atomic replacement,
        // owner-only permissions on Unix, outside Git-synced Project data.
        let save = || -> anyhow::Result<()> {
            std::fs::create_dir_all(directory)?;
            let mut file = tempfile::Builder::new()
                .prefix(".speech-settings-")
                .tempfile_in(directory)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                file.as_file()
                    .set_permissions(std::fs::Permissions::from_mode(0o600))?;
            }
            file.write_all(&serde_json::to_vec(self)?)?;
            file.as_file().sync_all()?;
            file.persist(directory.join(SETTINGS_FILE))?;
            Ok(())
        };
        save().map_err(|_| SpeechError::internal("Could not save speech settings"))
    }

    fn validate(&mut self) -> Result<(), SpeechError> {
        let mut url = reqwest::Url::parse(self.base_url.trim())
            .map_err(|_| SpeechError::bad_request("Enter a valid speech API base URL"))?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(SpeechError::bad_request(
                "Use an HTTP or HTTPS base URL without credentials, a query, or a fragment",
            ));
        }
        url.set_path(url.path().trim_end_matches('/').to_owned().as_str());
        self.base_url = url.as_str().trim_end_matches('/').to_owned();
        for (name, value) in [
            ("Transcription model", &mut self.stt_model),
            ("Speech model", &mut self.tts_model),
            ("Voice", &mut self.voice),
        ] {
            *value = value.trim().to_owned();
            if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
                return Err(SpeechError::bad_request(format!(
                    "{name} must contain 1–256 characters"
                )));
            }
        }
        self.api_key = self.api_key.trim().to_owned();
        if self.api_key.len() > 8192
            || HeaderValue::from_str(&format!("Bearer {}", self.api_key)).is_err()
        {
            return Err(SpeechError::bad_request("Invalid speech API key"));
        }
        Ok(())
    }

    fn request(&self, endpoint: &str) -> Result<reqwest::RequestBuilder, SpeechError> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120))
            // Never forward the saved key or an audio upload through redirects.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| SpeechError::internal("Could not initialize speech service"))?;
        let mut request = client.post(format!("{}/audio/{endpoint}", self.base_url));
        if !self.api_key.is_empty() {
            request = request.bearer_auth(&self.api_key);
        }
        Ok(request)
    }
}

pub(super) async fn get_settings(
    State(state): State<AppState>,
) -> Result<Json<SpeechSettings>, SpeechError> {
    Ok(Json(
        SpeechConfig::load(&state.config().system.data_dir)?.into(),
    ))
}

pub(super) async fn put_settings(
    State(state): State<AppState>,
    Json(input): Json<UpdateSpeechSettings>,
) -> Result<Json<SpeechSettings>, SpeechError> {
    let _guard = state.config_write_lock.lock().await;
    let directory = &state.config().system.data_dir;
    let saved = SpeechConfig::load(directory)?;
    let mut config = SpeechConfig {
        enabled: input.enabled,
        base_url: input.base_url,
        api_key: input.api_key.unwrap_or(saved.api_key),
        stt_model: input.stt_model,
        tts_model: input.tts_model,
        voice: input.voice,
    };
    config.validate()?;
    config.save(directory)?;
    Ok(Json(config.into()))
}

fn enabled_config(state: &AppState) -> Result<SpeechConfig, SpeechError> {
    let mut config = SpeechConfig::load(&state.config().system.data_dir)?;
    if !config.enabled {
        return Err(SpeechError(
            StatusCode::CONFLICT,
            "Enable speech in Settings → Speech first".into(),
        ));
    }
    config.validate()?;
    Ok(config)
}

fn audio_extension(mime: &str) -> Option<&'static str> {
    match mime {
        "audio/webm" | "video/webm" => Some("webm"),
        "audio/mp4" | "video/mp4" | "audio/m4a" | "audio/x-m4a" => Some("mp4"),
        "audio/ogg" => Some("ogg"),
        "audio/wav" | "audio/x-wav" | "audio/wave" => Some("wav"),
        "audio/mpeg" | "audio/mp3" => Some("mp3"),
        "audio/flac" | "audio/x-flac" => Some("flac"),
        _ => None,
    }
}

async fn transcribe(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<Json<serde_json::Value>, SpeechError> {
    let config = enabled_config(&state)?;
    let mut audio = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| SpeechError::bad_request("Invalid or oversized audio upload"))?
    {
        if field.name() != Some("file") || audio.is_some() {
            return Err(SpeechError::bad_request(
                "Upload a single audio file in the file field",
            ));
        }
        let mime = field
            .content_type()
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        let extension = audio_extension(&mime).ok_or_else(|| {
            SpeechError::bad_request(
                "Unsupported audio format. Use WebM, MP4, OGG, WAV, MP3, or FLAC",
            )
        })?;
        let bytes = field
            .bytes()
            .await
            .map_err(|_| SpeechError::bad_request("Invalid or oversized audio upload"))?;
        if bytes.is_empty() || bytes.len() > MAX_AUDIO_BYTES {
            return Err(SpeechError::bad_request(
                "Audio must be non-empty and no larger than 25 MiB",
            ));
        }
        let file = reqwest::multipart::Part::bytes(bytes.to_vec())
            .file_name(format!("recording.{extension}"))
            .mime_str(&mime)
            .map_err(|_| SpeechError::bad_request("Invalid audio MIME type"))?;
        audio = Some(file);
    }
    let file = audio.ok_or_else(|| SpeechError::bad_request("An audio file is required"))?;
    let form = reqwest::multipart::Form::new()
        .part("file", file)
        .text("model", config.stt_model.clone())
        .text("response_format", "json");
    let response = send(config.request("transcriptions")?.multipart(form)).await?;
    let bytes = bounded_body(response, MAX_TRANSCRIPT_BYTES).await?;
    let body: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| {
        SpeechError::upstream("Speech provider returned an invalid transcription response")
    })?;
    let text = body
        .get("text")
        .and_then(|value| value.as_str())
        .ok_or_else(|| SpeechError::upstream("Speech provider returned no transcription text"))?;
    Ok(Json(json!({ "text": text })))
}

#[derive(Deserialize)]
struct SpeechInput {
    input: String,
}

async fn synthesize(
    State(state): State<AppState>,
    Json(input): Json<SpeechInput>,
) -> Result<Response, SpeechError> {
    let config = enabled_config(&state)?;
    let text = input.input.trim();
    if text.is_empty() || text.chars().count() > MAX_SPEECH_CHARACTERS {
        return Err(SpeechError::bad_request(
            "Speech text must contain 1–4096 characters",
        ));
    }
    let response = send(config.request("speech")?.json(&json!({
        "model": config.tts_model,
        "voice": config.voice,
        "input": text,
        "response_format": "mp3",
    })))
    .await?;
    let mime = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("audio/mpeg")
        .split(';')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if !mime.starts_with("audio/") && mime != "application/octet-stream" {
        return Err(SpeechError::upstream(
            "Speech provider did not return audio",
        ));
    }
    let bytes = bounded_body(response, MAX_SPEECH_BYTES).await?;
    if bytes.is_empty() {
        return Err(SpeechError::upstream(
            "Speech provider returned empty audio",
        ));
    }
    Ok((
        [
            (header::CONTENT_TYPE, "audio/mpeg"),
            (header::CACHE_CONTROL, "no-store"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        Body::from(bytes),
    )
        .into_response())
}

async fn send(request: reqwest::RequestBuilder) -> Result<reqwest::Response, SpeechError> {
    let response = request.send().await.map_err(provider_connection_error)?;
    if !response.status().is_success() {
        // Provider error bodies can echo credentials or submitted text. Return
        // the status only, never arbitrary upstream content.
        return Err(SpeechError::upstream(format!(
            "Speech provider returned HTTP {}. Check the endpoint, API key, and model in Settings → Speech",
            response.status().as_u16()
        )));
    }
    Ok(response)
}

fn provider_connection_error(error: reqwest::Error) -> SpeechError {
    if error.is_timeout() {
        SpeechError(
            StatusCode::GATEWAY_TIMEOUT,
            "Speech provider request timed out".into(),
        )
    } else {
        SpeechError::upstream("Could not connect to the speech provider. Check Settings → Speech")
    }
}

async fn bounded_body(mut response: reqwest::Response, limit: usize) -> Result<Bytes, SpeechError> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(provider_connection_error)? {
        if chunk.len() > limit.saturating_sub(body.len()) {
            return Err(SpeechError::upstream(
                "Speech provider response is too large",
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body.into())
}

#[derive(Debug)]
pub(super) struct SpeechError(StatusCode, String);

impl SpeechError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self(StatusCode::BAD_REQUEST, message.into())
    }
    fn internal(message: impl Into<String>) -> Self {
        Self(StatusCode::INTERNAL_SERVER_ERROR, message.into())
    }
    fn upstream(message: impl Into<String>) -> Self {
        Self(StatusCode::BAD_GATEWAY, message.into())
    }
}

impl IntoResponse for SpeechError {
    fn into_response(self) -> Response {
        (
            self.0,
            [(header::CACHE_CONTROL, "no-store")],
            Json(json!({ "error": self.1 })),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderMap, Request};
    use http_body_util::BodyExt;
    use std::sync::Arc;
    use tower::ServiceExt;
    use xpressclaw_core::{config::Config, db::Database};

    fn state() -> (AppState, tempfile::TempDir) {
        let root = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.system.data_dir = root.path().into();
        config.system.workspace_dir = root.path().join("workspaces");
        let state = AppState::new(
            Arc::new(config),
            Arc::new(Database::open_memory().unwrap()),
            None,
            root.path().join("config.yaml"),
            true,
        );
        (state, root)
    }

    async fn body(response: Response) -> Bytes {
        response.into_body().collect().await.unwrap().to_bytes()
    }

    async fn json_body(response: Response) -> serde_json::Value {
        serde_json::from_slice(&body(response).await).unwrap()
    }

    async fn put(state: AppState, input: serde_json::Value) -> Response {
        super::super::settings::routes()
            .with_state(state)
            .oneshot(
                Request::put("/speech")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(input.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    fn enable(state: &AppState, url: String) {
        SpeechConfig {
            enabled: true,
            base_url: url,
            api_key: "secret-provider-key".into(),
            stt_model: "custom-transcriber".into(),
            tts_model: "custom-speaker".into(),
            voice: "custom-voice".into(),
        }
        .save(&state.config().system.data_dir)
        .unwrap();
    }

    async fn speech(state: AppState, input: &str) -> Response {
        routes()
            .with_state(state)
            .oneshot(
                Request::post("/synthesize")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({ "input": input }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn transcription(state: AppState, mime: &str, bytes: &[u8]) -> Response {
        let mut upload = format!("--boundary\r\nContent-Disposition: form-data; name=\"file\"; filename=\"untrusted-name\"\r\nContent-Type: {mime}\r\n\r\n").into_bytes();
        upload.extend_from_slice(bytes);
        upload.extend_from_slice(b"\r\n--boundary--\r\n");
        routes()
            .with_state(state)
            .oneshot(
                Request::post("/transcriptions")
                    .header(
                        header::CONTENT_TYPE,
                        "multipart/form-data; boundary=boundary",
                    )
                    .body(Body::from(upload))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    struct Provider {
        url: String,
        task: tokio::task::JoinHandle<()>,
    }
    impl Drop for Provider {
        fn drop(&mut self) {
            self.task.abort();
        }
    }
    async fn provider(router: Router) -> Provider {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/compatible/v1", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Provider { url, task }
    }

    #[tokio::test]
    async fn settings_preserve_clear_and_never_return_saved_key() {
        let (state, root) = state();
        let mut input = json!({"enabled": true, "base_url": "http://localhost:9000/v1/", "api_key": "secret-provider-key", "stt_model": "stt", "tts_model": "tts", "voice": "voice"});
        let response = put(state.clone(), input.clone()).await;
        assert_eq!(response.status(), StatusCode::OK);
        let value = json_body(response).await;
        assert_eq!(value["has_api_key"], true);
        assert_eq!(value["base_url"], "http://localhost:9000/v1");
        assert!(!value.to_string().contains("secret-provider-key"));
        assert!(value.get("api_key").is_none());
        input.as_object_mut().unwrap().remove("api_key");
        input["voice"] = json!("updated-voice");
        assert_eq!(
            put(state.clone(), input.clone()).await.status(),
            StatusCode::OK
        );
        let reloaded = SpeechConfig::load(root.path()).unwrap();
        assert_eq!(reloaded.api_key, "secret-provider-key");
        assert_eq!(reloaded.voice, "updated-voice");
        let Json(settings) = get_settings(State(state.clone())).await.unwrap();
        assert!(!serde_json::to_string(&settings)
            .unwrap()
            .contains("secret-provider-key"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(root.path().join(SETTINGS_FILE))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        input["api_key"] = json!("");
        assert_eq!(
            json_body(put(state, input).await).await["has_api_key"],
            false
        );
        assert!(SpeechConfig::load(root.path()).unwrap().api_key.is_empty());
    }

    #[tokio::test]
    async fn invalid_settings_do_not_replace_saved_credentials() {
        let (state, root) = state();
        enable(&state, "http://localhost/v1".into());
        for url in [
            "file:///tmp/provider",
            "https://user:password@example.com/v1",
            "https://example.com/v1?api_key=secret",
            "https://example.com/#fragment",
            "not-a-url",
        ] {
            let response = put(state.clone(), json!({"enabled":true,"base_url":url,"api_key":"replacement","stt_model":"stt","tts_model":"tts","voice":"voice"})).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{url}");
        }
        assert_eq!(
            SpeechConfig::load(root.path()).unwrap().api_key,
            "secret-provider-key"
        );
    }

    #[tokio::test]
    async fn transcription_forwards_browser_audio_as_openai_multipart() {
        let (state, _root) = state();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let upstream = provider(Router::new().route("/compatible/v1/audio/transcriptions", post(move |headers: HeaderMap, mut upload: Multipart| {
            let tx = tx.clone();
            async move {
                let mut fields = json!({"authorization":headers.get(header::AUTHORIZATION).unwrap().to_str().unwrap()});
                while let Some(field) = upload.next_field().await.unwrap() {
                    let name = field.name().unwrap().to_owned();
                    if name == "file" {
                        fields["filename"] = json!(field.file_name());
                        fields["mime"] = json!(field.content_type());
                    }
                    fields[name] = json!(field.text().await.unwrap());
                }
                tx.send(fields).unwrap();
                Json(json!({"text":"A dictated message.","ignored_provider_metadata":true}))
            }
        }))).await;
        enable(&state, upstream.url.clone());
        for (mime, extension) in [("audio/webm;codecs=opus", "webm"), ("audio/mp4", "mp4")] {
            let response = transcription(state.clone(), mime, b"audio-data").await;
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                json_body(response).await,
                json!({"text":"A dictated message."})
            );
            let forwarded = rx.recv().await.unwrap();
            assert_eq!(forwarded["authorization"], "Bearer secret-provider-key");
            assert_eq!(forwarded["model"], "custom-transcriber");
            assert_eq!(forwarded["response_format"], "json");
            assert_eq!(forwarded["filename"], format!("recording.{extension}"));
            assert_eq!(forwarded["mime"], mime.split(';').next().unwrap());
            assert_eq!(forwarded["file"], "audio-data");
        }
    }

    #[tokio::test]
    async fn speech_uses_saved_model_voice_and_returns_uncached_audio() {
        let (state, root) = state();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let upstream = provider(Router::new().route(
            "/compatible/v1/audio/speech",
            post(
                move |headers: HeaderMap, Json(input): Json<serde_json::Value>| {
                    let tx = tx.clone();
                    async move {
                        tx.send((headers.contains_key(header::AUTHORIZATION), input))
                            .unwrap();
                        ([(header::CONTENT_TYPE, "audio/mpeg")], b"fake-mp3".to_vec())
                    }
                },
            ),
        ))
        .await;
        enable(&state, upstream.url.clone());
        // Local compatible services may not require an API key.
        let mut config = SpeechConfig::load(root.path()).unwrap();
        config.api_key.clear();
        config.save(root.path()).unwrap();
        let response = speech(state, " Read this aloud. ").await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "audio/mpeg");
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(body(response).await, "fake-mp3");
        let (authenticated, forwarded) = rx.recv().await.unwrap();
        assert!(!authenticated);
        assert_eq!(
            forwarded,
            json!({"input":"Read this aloud.","model":"custom-speaker","voice":"custom-voice","response_format":"mp3"})
        );
    }

    #[tokio::test]
    async fn disabled_and_invalid_input_never_reach_provider() {
        let (state, _root) = state();
        assert_eq!(
            speech(state.clone(), "hello").await.status(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            transcription(state.clone(), "audio/webm", b"data")
                .await
                .status(),
            StatusCode::CONFLICT
        );
        enable(&state, "http://127.0.0.1:1/v1".into());
        for input in [String::new(), " ".into(), "a".repeat(4097)] {
            assert_eq!(
                speech(state.clone(), &input).await.status(),
                StatusCode::BAD_REQUEST
            );
        }
        for (mime, data) in [
            ("text/plain", b"data".as_slice()),
            ("audio/webm", b"".as_slice()),
        ] {
            assert_eq!(
                transcription(state.clone(), mime, data).await.status(),
                StatusCode::BAD_REQUEST
            );
        }
        assert_eq!(
            transcription(state, "audio/webm", &vec![b'x'; MAX_AUDIO_BYTES + 1])
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn upstream_errors_redirects_and_bad_responses_are_contained() {
        let (state, _root) = state();
        for status in [
            StatusCode::UNAUTHORIZED,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::TEMPORARY_REDIRECT,
        ] {
            let upstream = provider(Router::new().route(
                "/compatible/v1/audio/speech",
                post(move || async move {
                    (
                        status,
                        [(header::LOCATION, "http://127.0.0.1:1/steal-key")],
                        "secret-provider-key and private input",
                    )
                }),
            ))
            .await;
            enable(&state, upstream.url.clone());
            let response = speech(state.clone(), "hello").await;
            assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
            let error = json_body(response).await["error"]
                .as_str()
                .unwrap()
                .to_owned();
            assert!(error.contains(&status.as_u16().to_string()));
            assert!(!error.contains("secret-provider-key"));
        }
        let upstream = provider(
            Router::new()
                .route(
                    "/compatible/v1/audio/speech",
                    post(|| async { Json(json!({"error":"not audio"})) }),
                )
                .route(
                    "/compatible/v1/audio/transcriptions",
                    post(|| async { Json(json!({"unexpected":true})) }),
                ),
        )
        .await;
        enable(&state, upstream.url.clone());
        assert_eq!(
            speech(state.clone(), "hello").await.status(),
            StatusCode::BAD_GATEWAY
        );
        assert_eq!(
            transcription(state, "audio/webm", b"data").await.status(),
            StatusCode::BAD_GATEWAY
        );
    }

    #[tokio::test]
    async fn provider_response_size_is_bounded() {
        let upstream =
            provider(Router::new().route("/large", post(|| async { "too much data" }))).await;
        let response = reqwest::Client::new()
            .post(upstream.url.replace("/compatible/v1", "/large"))
            .send()
            .await
            .unwrap();
        assert_eq!(
            bounded_body(response, 4).await.unwrap_err().0,
            StatusCode::BAD_GATEWAY
        );
    }
}
