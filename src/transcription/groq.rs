//! Groq hosted-Whisper backend (R8, R12).
//!
//! Encodes the canonical 16 kHz mono buffer to an in-memory WAV, POSTs it as
//! multipart with a Bearer token, and parses `{"text": "..."}`. One reused
//! `reqwest::Client` with concrete 5 s connect / 15 s total timeouts (KTD4) so a
//! stalled network cannot lock the single-in-flight state machine.

use std::io::Cursor;
use std::time::Duration;

use reqwest::multipart::{Form, Part};
use reqwest::Client;
use serde::Deserialize;

use super::TranscribeError;
use crate::config::Secret;

const ENDPOINT: &str = "https://api.groq.com/openai/v1/audio/transcriptions";
const MODEL: &str = "whisper-large-v3-turbo";
/// Groq free-tier upload limit; pre-checked before sending.
const MAX_UPLOAD_BYTES: usize = 25 * 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Deserialize)]
struct GroqResponse {
    text: String,
}

#[derive(Deserialize)]
struct GroqErrorEnvelope {
    error: GroqErrorDetail,
}

#[derive(Deserialize)]
struct GroqErrorDetail {
    message: String,
}

#[derive(Clone)]
pub struct GroqBackend {
    client: Client,
    api_key: Secret,
}

impl GroqBackend {
    pub fn new(api_key: Secret) -> Self {
        let client = Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .unwrap_or_else(|_| Client::new());
        GroqBackend { client, api_key }
    }

    pub async fn transcribe(&self, audio: &[f32]) -> Result<String, TranscribeError> {
        let wav = encode_wav_16k_mono(audio)?;
        if wav.len() > MAX_UPLOAD_BYTES {
            return Err(TranscribeError::TooLarge(wav.len()));
        }

        let part = Part::bytes(wav)
            .file_name("audio.wav")
            .mime_str("audio/wav")
            .map_err(|e| TranscribeError::Network(e.to_string()))?;
        let form = Form::new()
            .part("file", part)
            .text("model", MODEL)
            .text("response_format", "json")
            .text("language", "en");

        let resp = self
            .client
            .post(ENDPOINT)
            .bearer_auth(self.api_key.expose())
            .multipart(form)
            .send()
            .await
            .map_err(map_reqwest_error)?;

        let status = resp.status();
        if status.is_success() {
            let parsed: GroqResponse = resp
                .json()
                .await
                .map_err(|e| TranscribeError::Decode(e.to_string()))?;
            return Ok(parsed.text.trim().to_string());
        }

        // Capture the retry hint before the body consumes the response.
        let retry_after = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok());
        let code = status.as_u16();
        let body = resp.text().await.unwrap_or_default();
        Err(classify_error(code, retry_after, body))
    }
}

/// Build a 16 kHz mono 16-bit PCM WAV from the canonical f32 buffer.
fn encode_wav_16k_mono(samples: &[f32]) -> Result<Vec<u8>, TranscribeError> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut cursor = Cursor::new(Vec::<u8>::new());
    {
        let mut writer = hound::WavWriter::new(&mut cursor, spec)
            .map_err(|e| TranscribeError::Encode(e.to_string()))?;
        for &s in samples {
            let v = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
            writer
                .write_sample(v)
                .map_err(|e| TranscribeError::Encode(e.to_string()))?;
        }
        writer
            .finalize()
            .map_err(|e| TranscribeError::Encode(e.to_string()))?;
    }
    Ok(cursor.into_inner())
}

fn map_reqwest_error(err: reqwest::Error) -> TranscribeError {
    if err.is_timeout() {
        TranscribeError::Timeout
    } else {
        TranscribeError::Network(err.to_string())
    }
}

/// Map an HTTP error status + body to a typed error, preferring the Groq error
/// envelope's message when present.
fn classify_error(code: u16, retry_after: Option<u64>, body: String) -> TranscribeError {
    let message = serde_json::from_str::<GroqErrorEnvelope>(&body)
        .map(|e| e.error.message)
        .unwrap_or(body);
    match code {
        401 | 403 => TranscribeError::Auth(message),
        413 => TranscribeError::TooLarge(0),
        429 => TranscribeError::RateLimited {
            retry_after,
            message,
        },
        500..=599 => TranscribeError::Server(message),
        _ => TranscribeError::Http(code, message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn successful_body_deserializes_to_text() {
        let parsed: GroqResponse = serde_json::from_str(r#"{"text":"hello world"}"#).unwrap();
        assert_eq!(parsed.text, "hello world");
    }

    #[test]
    fn wav_encoding_produces_valid_16k_mono_header() {
        let samples = vec![0.0f32, 0.5, -0.5, 1.0, -1.0];
        let bytes = encode_wav_16k_mono(&samples).unwrap();
        // Parse it back with hound to verify the header is valid + correct.
        let reader = hound::WavReader::new(Cursor::new(bytes)).unwrap();
        let spec = reader.spec();
        assert_eq!(spec.channels, 1);
        assert_eq!(spec.sample_rate, 16_000);
        assert_eq!(spec.bits_per_sample, 16);
        assert_eq!(reader.len(), samples.len() as u32);
    }

    #[test]
    fn auth_status_maps_to_auth_error() {
        let err = classify_error(
            401,
            None,
            r#"{"error":{"message":"Invalid API Key"}}"#.into(),
        );
        assert!(matches!(err, TranscribeError::Auth(m) if m == "Invalid API Key"));
    }

    #[test]
    fn rate_limit_carries_retry_after() {
        let err = classify_error(429, Some(7), "{}".into());
        assert!(matches!(
            err,
            TranscribeError::RateLimited {
                retry_after: Some(7),
                ..
            }
        ));
    }

    #[test]
    fn server_error_is_transient() {
        let err = classify_error(503, None, "upstream down".into());
        assert!(matches!(err, TranscribeError::Server(_)));
    }

    #[test]
    fn unparseable_error_body_falls_back_to_raw() {
        let err = classify_error(400, None, "not json".into());
        assert!(matches!(err, TranscribeError::Http(400, m) if m == "not json"));
    }
}
