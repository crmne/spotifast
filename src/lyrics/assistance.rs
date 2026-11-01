//! Optional lyric translations and local, approximate pronunciation guides.

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use serde::Deserialize;
use sha1::{Digest, Sha1};

use super::Lyrics;

const API: &str = "https://api.mymemory.translated.net/get";
const CACHE_SIZE: usize = 32;
/// Serialize requests and reuse recent songs without spending the daily quota again.
struct Cache {
    songs: VecDeque<(String, Vec<String>)>,
    blocked: Option<(Option<tokio::time::Instant>, Failure)>,
}
static CACHE: tokio::sync::Mutex<Cache> = tokio::sync::Mutex::const_new(Cache {
    songs: VecDeque::new(),
    blocked: None,
});

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    Unavailable,
    DailyLimit,
    RateLimited(Duration),
    LineTooLong,
    InvalidResponse,
    UnknownLanguage,
}

#[derive(Default)]
pub enum Translation {
    #[default]
    Idle,
    Loading,
    Ready(Vec<String>),
    Failed(Failure),
}

/// View state only. Nothing is added to the persistent Settings file.
#[derive(Default)]
pub struct Assistance {
    pub show_translation: bool,
    pub show_pronunciation: bool,
    pub translation: Translation,
    pub pronunciation: Vec<Option<String>>,
    pub generation: u64,
    pub target: String,
}

impl Assistance {
    pub fn reset(&mut self, lyrics: Option<&Lyrics>) {
        self.generation = self.generation.wrapping_add(1);
        self.translation = Translation::Idle;
        self.pronunciation = lyrics.map(readings).unwrap_or_default();
    }

    pub fn receive(&mut self, generation: u64, result: Result<Vec<String>, Failure>) -> bool {
        if self.generation != generation {
            return false;
        }
        self.translation = match result {
            Ok(lines) => Translation::Ready(lines),
            Err(error) => Translation::Failed(error),
        };
        true
    }
}

/// Spotify supplies a language. LRCLIB/local files do not: Hangul is
/// unambiguous enough to support Korean there without guessing other languages.
pub fn language(lyrics: &Lyrics) -> Option<&str> {
    lyrics
        .language
        .as_deref()
        .filter(|tag| valid_language(tag))
        .or_else(|| {
            lyrics
                .lines
                .iter()
                .any(|line| {
                    line.text
                        .chars()
                        .any(|c| matches!(c, '\u{ac00}'..='\u{d7a3}'))
                })
                .then_some("ko")
        })
}

fn valid_language(tag: &str) -> bool {
    (2..=16).contains(&tag.len())
        && tag
            .split('-')
            .all(|part| !part.is_empty() && part.bytes().all(|c| c.is_ascii_alphabetic()))
        && !matches!(tag, "und" | "zxx")
}

pub fn readings(lyrics: &Lyrics) -> Vec<Option<String>> {
    let scheme = match language(lyrics).and_then(|tag| tag.split('-').next()) {
        Some("ko") => "rr",
        Some("ru" | "bg" | "be" | "mk" | "sr") => "gost",
        Some("uk") => "uk",
        Some("el") => "elot",
        Some("ar" | "fa") => "din31635",
        Some("he") => "heb",
        Some("hi" | "sa") => "iast",
        Some("ja") => "hepburn",
        _ => return vec![None; lyrics.lines.len()],
    };
    lyrics
        .lines
        .iter()
        .map(|line| {
            transliteratio::transliterate(&line.text, scheme)
                .ok()
                .filter(|reading| reading != &line.text)
        })
        .collect()
}

/// The app's language tags, expressed as MyMemory's RFC 3066 tags.
pub fn target_language(tag: &str) -> &str {
    match tag {
        "zh-Hans" => "zh-CN",
        "zh-Hant" => "zh-TW",
        tag => tag,
    }
}

pub async fn translate(
    http: &reqwest::Client,
    lyrics: &Lyrics,
    target: &str,
) -> Result<Vec<String>, Failure> {
    let source = language(lyrics).ok_or(Failure::UnknownLanguage)?;
    let target = target_language(target);
    let key = format!(
        "{:x}",
        Sha1::digest(
            serde_json::to_vec(&(source, target, &lyrics.lines))
                .map_err(|_| Failure::InvalidResponse)?
        )
    );
    let mut cache = CACHE.lock().await;
    if let Some((_, lines)) = cache.songs.iter().find(|(held, _)| held == &key) {
        return Ok(lines.clone());
    }
    if let Some((until, failure)) = cache.blocked
        && until.is_none_or(|until| until > tokio::time::Instant::now())
    {
        return Err(failure);
    }
    let result = tokio::time::timeout(
        Duration::from_secs(90),
        translate_from(http, API, lyrics, source, target),
    )
    .await
    .map_err(|_| Failure::Unavailable)?;
    match result {
        Ok(lines) => {
            cache.songs.push_back((key, lines.clone()));
            if cache.songs.len() > CACHE_SIZE {
                cache.songs.pop_front();
            }
            Ok(lines)
        }
        Err(error) => {
            let wait = match error {
                Failure::DailyLimit => Some(Duration::from_secs(60 * 60)),
                Failure::RateLimited(wait) => Some(wait),
                _ => None,
            };
            if let Some(wait) = wait {
                // An unparseable Retry-After is Duration::MAX. Keep the
                // pause for this run rather than silently bypassing it.
                cache.blocked = Some((tokio::time::Instant::now().checked_add(wait), error));
            }
            Err(error)
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Response {
    response_status: u16,
    #[serde(default)]
    quota_finished: bool,
    response_data: Option<ResponseData>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResponseData {
    translated_text: String,
}

async fn translate_from(
    http: &reqwest::Client,
    api: &str,
    lyrics: &Lyrics,
    source: &str,
    target: &str,
) -> Result<Vec<String>, Failure> {
    if source.split('-').next() == target.split('-').next() {
        return Ok(lyrics.lines.iter().map(|line| line.text.clone()).collect());
    }
    // Validate before spending quota on a song that cannot be completed.
    if lyrics.lines.iter().any(|line| line.text.len() > 500) {
        return Err(Failure::LineTooLong);
    }
    let mut unique: HashMap<String, String> = HashMap::new();
    let mut output = Vec::with_capacity(lyrics.lines.len());
    for line in &lyrics.lines {
        if line.text.trim().is_empty() || line.text.trim() == "♪" {
            output.push(String::new());
            continue;
        }
        if let Some(translation) = unique.get(&line.text) {
            output.push(translation.clone());
            continue;
        }
        let response = http
            .get(api)
            .query(&[
                ("q", line.text.as_str()),
                ("langpair", &format!("{source}|{target}")),
            ])
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .map_err(|_| Failure::Unavailable)?;
        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(Failure::RateLimited(super::retry_after(response.headers())));
        }
        let body: Response = response
            .error_for_status()
            .map_err(|_| Failure::Unavailable)?
            .json()
            .await
            .map_err(|_| Failure::InvalidResponse)?;
        if body.quota_finished || body.response_status == 429 {
            return Err(Failure::DailyLimit);
        }
        if body.response_status != 200 {
            return Err(Failure::Unavailable);
        }
        let translation = body
            .response_data
            .ok_or(Failure::InvalidResponse)?
            .translated_text;
        if translation.trim().is_empty() {
            return Err(Failure::InvalidResponse);
        }
        let translation = html_escape::decode_html_entities(&translation).into_owned();
        unique.insert(line.text.clone(), translation.clone());
        output.push(translation);
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn korean() -> Lyrics {
        super::super::from_text("[00:01]한글\n[00:02]\n[00:03]한글").unwrap()
    }

    #[test]
    fn pronunciation_preserves_line_alignment_and_latin_text() {
        let mut lyrics = korean();
        lyrics.lines.push(super::super::Line {
            at_ms: Some(4000),
            text: "Hello!".into(),
        });
        assert_eq!(language(&lyrics), Some("ko"));
        assert_eq!(
            readings(&lyrics),
            vec![Some("hangeul".into()), None, Some("hangeul".into()), None]
        );
        assert_eq!(lyrics.active_line(3000), Some(2));
    }

    #[test]
    fn older_cache_files_and_unknown_languages_are_safe() {
        let lyrics: Lyrics =
            serde_json::from_str(r#"{"lines":[],"synced":false,"instrumental":false}"#).unwrap();
        assert_eq!(language(&lyrics), None);
        assert_eq!(target_language("zh-Hant"), "zh-TW");
        assert!(!valid_language("und"));
        assert!(!valid_language("ko|nl"));
    }

    #[test]
    fn stale_results_cannot_replace_a_new_song() {
        let mut state = Assistance::default();
        state.reset(Some(&korean()));
        let first = state.generation;
        state.reset(None);
        assert!(!state.receive(first, Ok(vec!["old".into()])));
        assert!(matches!(state.translation, Translation::Idle));
        assert!(state.receive(state.generation, Err(Failure::DailyLimit)));
        assert!(matches!(
            state.translation,
            Translation::Failed(Failure::DailyLimit)
        ));
    }

    async fn server(body: &'static str) -> (String, tokio::task::JoinHandle<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            let n = socket.read(&mut request).await.unwrap();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            String::from_utf8_lossy(&request[..n]).into_owned()
        });
        (format!("http://{address}"), task)
    }

    #[tokio::test]
    async fn duplicate_lines_only_use_one_request_and_keep_blank_timing_slots() {
        let (api, request) = server(
            r#"{"responseStatus":200,"responseData":{"translatedText":"Korean &amp; letters"}}"#,
        )
        .await;
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let lines = translate_from(&http, &api, &korean(), "ko", "en")
            .await
            .unwrap();
        assert_eq!(lines, ["Korean & letters", "", "Korean & letters"]);
        let request = request.await.unwrap();
        assert!(request.contains("langpair=ko%7Cen"));
        assert!(!request.to_lowercase().contains("authorization:"));
    }

    #[tokio::test]
    async fn quota_messages_are_errors_not_lyrics() {
        let (api, request) = server(r#"{"responseStatus":403,"quotaFinished":true,"responseData":{"translatedText":"LIMIT REACHED"}}"#).await;
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        assert_eq!(
            translate_from(&http, &api, &korean(), "ko", "en").await,
            Err(Failure::DailyLimit)
        );
        request.await.unwrap();
    }

    #[tokio::test]
    async fn oversized_utf8_lines_fail_before_a_request() {
        let lyrics = super::super::from_text(&"한".repeat(167)).unwrap();
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        assert_eq!(
            translate_from(&http, "http://127.0.0.1:1", &lyrics, "ko", "en").await,
            Err(Failure::LineTooLong)
        );
    }
}
