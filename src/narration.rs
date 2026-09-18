//! Spotify AI DJ (DJ X) TTS voice narration synthesis and audio playback.

use crate::sink::AudioControl;
use std::collections::HashMap;
use std::sync::Arc;

fn encode_varint(mut val: u64, buf: &mut Vec<u8>) {
    while val >= 0x80 {
        buf.push((val as u8 & 0x7f) | 0x80);
        val >>= 7;
    }
    buf.push(val as u8);
}

/// Encodes a protobuf TtsRequest for Spotify's /client-tts/v1/fulfill endpoint.
pub fn encode_tts_request(
    ssml: &str,
    voice: u32,
    provider: u32,
    sample_rate: u32,
    format: u32,
) -> Vec<u8> {
    let mut buf = Vec::new();
    // Field 2: prompt (ssml) - wire type 2 (length delimited)
    encode_varint((2 << 3) | 2, &mut buf);
    encode_varint(ssml.len() as u64, &mut buf);
    buf.extend_from_slice(ssml.as_bytes());

    // Field 3: audio_format - wire type 0 (varint)
    encode_varint(3 << 3, &mut buf);
    encode_varint(format as u64, &mut buf);

    // Field 5: tts_voice - wire type 0 (varint)
    encode_varint(5 << 3, &mut buf);
    encode_varint(voice as u64, &mut buf);

    // Field 6: tts_provider - wire type 0 (varint)
    encode_varint(6 << 3, &mut buf);
    encode_varint(provider as u64, &mut buf);

    // Field 7: sample_rate_hz - wire type 0 (varint)
    encode_varint(7 << 3, &mut buf);
    encode_varint(sample_rate as u64, &mut buf);

    buf
}

pub fn parse_voice(voice_str: Option<&str>) -> u32 {
    match voice_str {
        Some(s) if s.starts_with("VOICE") => s[5..].parse::<u32>().unwrap_or(1),
        _ => 1,
    }
}

pub fn parse_provider(provider_str: Option<&str>) -> u32 {
    match provider_str {
        Some("SONANTIC_FAST") => 6,
        Some("SONANTIC_DEPRECATED") => 5,
        Some("WELL_SAID") => 4,
        Some("POLLY") => 3,
        Some("READSPEAKER") => 2,
        Some("CLOUD_TTS") => 1,
        _ => 6,
    }
}

/// Fetches the synthesized audio for the given SSML prompt from Spotify's client-tts service.
pub async fn fetch_narration_audio(
    token: &str,
    client_token: Option<&str>,
    ssml: &str,
    voice: u32,
    provider: u32,
) -> Result<Vec<u8>, anyhow::Error> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()?;

    // Spotify client-tts supports MP3 (5), VORBIS (4), and WAV (1)
    let formats = [5u32, 4u32, 1u32];
    let mut last_err = None;

    for &format in &formats {
        let body = encode_tts_request(ssml, voice, provider, 44100, format);
        let mut req = client
            .post("https://spclient.wg.spotify.com/client-tts/v1/fulfill")
            .header("Authorization", format!("Bearer {}", token))
            .header("Content-Type", "application/x-protobuf")
            .header("Accept", "application/octet-stream");

        if let Some(ct) = client_token {
            req = req.header("client-token", ct);
        }

        let res = match req.body(body).send().await {
            Ok(r) => r,
            Err(e) => {
                last_err = Some(anyhow::anyhow!("Request failed: {e}"));
                continue;
            }
        };

        let status = res.status();
        if status == reqwest::StatusCode::SEE_OTHER
            || status == reqwest::StatusCode::FOUND
            || status == reqwest::StatusCode::MOVED_PERMANENTLY
            || status == reqwest::StatusCode::TEMPORARY_REDIRECT
        {
            if let Some(loc) = res.headers().get(reqwest::header::LOCATION) {
                let url = loc.to_str()?;
                log::debug!("TTS audio redirect found: {url}");
                let audio_res = reqwest::get(url).await?;
                if audio_res.status().is_success() {
                    let bytes = audio_res.bytes().await?;
                    if !bytes.is_empty() {
                        return Ok(bytes.to_vec());
                    }
                } else {
                    last_err = Some(anyhow::anyhow!(
                        "Audio download failed with status {}",
                        audio_res.status()
                    ));
                }
            }
        } else if status.is_success() {
            let bytes = res.bytes().await?;
            if !bytes.is_empty() {
                return Ok(bytes.to_vec());
            }
        } else {
            let body_text = res.text().await.unwrap_or_default();
            log::warn!("client-tts fulfill returned status {status}: {body_text}");
            last_err = Some(anyhow::anyhow!("client-tts status {status}: {body_text}"));
        }
    }

    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("Unable to fulfill TTS narration request")))
}

/// Dispatches an asynchronous request to fetch and play track narration.
pub fn handle_track_narration(
    session: librespot_core::Session,
    audio: Arc<AudioControl>,
    uri: String,
    metadata: HashMap<String, String>,
) {
    let (prefix, ssml) = if let Some(ssml) = metadata.get("narration.intro.ssml") {
        ("narration.intro", ssml.clone())
    } else if let Some(ssml) = metadata.get("narration.jump.ssml") {
        ("narration.jump", ssml.clone())
    } else if let Some(ssml) = metadata.get("narration.outro.ssml") {
        ("narration.outro", ssml.clone())
    } else {
        audio.stop_narration();
        return;
    };

    if ssml.is_empty() {
        audio.stop_narration();
        return;
    }

    // Gated audio playback: stop any previous narration and wait until new narration completes
    audio.stop_narration();
    audio.begin_narration();

    let voice_key = format!("{prefix}.voice");
    let provider_key = format!("{prefix}.tts_provider");
    let voice = parse_voice(metadata.get(&voice_key).map(|s| s.as_str()));
    let provider = parse_provider(metadata.get(&provider_key).map(|s| s.as_str()));

    let audio_clone = Arc::clone(&audio);
    tokio::spawn(async move {
        let token = match session.login5().auth_token().await {
            Ok(t) => t.access_token,
            Err(_) => match session.token_provider().get_token("streaming").await {
                Ok(t) => t.access_token,
                Err(e) => {
                    log::warn!("No Spotify access token available for narration of {uri}: {e}");
                    audio_clone.finish_narration();
                    return;
                }
            },
        };

        let client_token = session.spclient().client_token().await.ok();

        log::info!("Synthesizing DJ commentary ({prefix}) for track {uri}");
        match fetch_narration_audio(&token, client_token.as_deref(), &ssml, voice, provider).await {
            Ok(bytes) => {
                log::info!("Received {} bytes of DJ commentary audio", bytes.len());
                if let Err(e) = audio_clone.play_narration(bytes) {
                    log::warn!("Failed to play DJ commentary: {e}");
                    audio_clone.finish_narration();
                }
            }
            Err(e) => {
                log::warn!("Failed to fetch DJ commentary: {e}");
                audio_clone.finish_narration();
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_voice() {
        assert_eq!(parse_voice(Some("VOICE1")), 1);
        assert_eq!(parse_voice(Some("VOICE2")), 2);
        assert_eq!(parse_voice(Some("VOICE15")), 15);
        assert_eq!(parse_voice(Some("VOICEXYZ")), 1);
        assert_eq!(parse_voice(Some("OTHER")), 1);
        assert_eq!(parse_voice(None), 1);
    }

    #[test]
    fn test_parse_provider() {
        assert_eq!(parse_provider(Some("SONANTIC_FAST")), 6);
        assert_eq!(parse_provider(Some("SONANTIC_DEPRECATED")), 5);
        assert_eq!(parse_provider(Some("WELL_SAID")), 4);
        assert_eq!(parse_provider(Some("POLLY")), 3);
        assert_eq!(parse_provider(Some("READSPEAKER")), 2);
        assert_eq!(parse_provider(Some("CLOUD_TTS")), 1);
        assert_eq!(parse_provider(Some("CUSTOM_FALLBACK")), 6);
        assert_eq!(parse_provider(None), 6);
    }

    #[test]
    fn test_encode_tts_request_structure() {
        let ssml = "<speak>Here is your mix</speak>";
        let voice = 1;
        let provider = 6;
        let sample_rate = 44100;
        let format = 5; // MP3

        let encoded = encode_tts_request(ssml, voice, provider, sample_rate, format);

        // Field 2 (prompt): tag (2 << 3) | 2 = 0x12
        assert_eq!(encoded[0], 0x12);
        assert_eq!(encoded[1], ssml.len() as u8);
        assert_eq!(&encoded[2..2 + ssml.len()], ssml.as_bytes());

        // Contains field tags:
        // Field 3: (3 << 3) | 0 = 0x18
        // Field 5: (5 << 3) | 0 = 0x28
        // Field 6: (6 << 3) | 0 = 0x30
        // Field 7: (7 << 3) | 0 = 0x38
        assert!(encoded.windows(2).any(|w| w == [0x18, format as u8]));
        assert!(encoded.windows(2).any(|w| w == [0x28, voice as u8]));
        assert!(encoded.windows(2).any(|w| w == [0x30, provider as u8]));
    }
}
