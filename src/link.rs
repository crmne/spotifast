//! Spotify links as they arrive from outside: the desktop's URL handler,
//! the command line, and a second launch handing one to the running
//! instance.
//!
//! Every shape Spotify hands out comes back as the one canonical URI,
//! `spotify:<kind>:<id>` or `spotify:search:<encoded query>`, or nothing
//! when it is not something the app can open.

/// Resource pages. Search links carry text instead of a resource id.
const KINDS: [&str; 7] = [
    "track",
    "album",
    "artist",
    "playlist",
    "show",
    "episode",
    "prerelease",
];

/// The canonical form of a context URI Spotify reports as playing.
/// Personalized playlists report their context with the owner embedded,
/// `spotify:user:NAME:playlist:ID`, while the app's models hold the plain
/// `spotify:playlist:ID`; strict comparisons need the one shape.
pub fn canonical_context_uri(uri: &str) -> String {
    if let Some(rest) = uri.strip_prefix("spotify:user:") {
        const PLAYLIST: &str = ":playlist:";
        if let Some(at) = rest.find(PLAYLIST) {
            let id = &rest[at + PLAYLIST.len()..];
            if !id.is_empty() && !id.contains(':') {
                return format!("spotify:playlist:{id}");
            }
        }
    }
    uri.to_owned()
}

/// The canonical `spotify:<kind>:<id>` behind `text`, or `None` when it is
/// not a link to a track, album, artist, playlist, show, episode, prerelease, or search.
///
/// Accepted: `spotify:track:ID`, the old `spotify:user:NAME:playlist:ID`,
/// `spotify://track/ID`, and `https://open.spotify.com/track/ID` with or
/// without a locale segment (`/intl-de/`), a query string, or the old
/// `/user/NAME/playlist/ID` shape.
pub fn parse(text: &str) -> Option<String> {
    if let Some(query) = search_query(text) {
        return Some(format!(
            "spotify:search:{}",
            percent_encoding::utf8_percent_encode(&query, percent_encoding::NON_ALPHANUMERIC)
        ));
    }
    let text = text.trim();
    let mut segments: Vec<&str> = if let Some(rest) = text.strip_prefix("spotify://") {
        // The URL shape of the URI: `spotify://track/ID`, or the web
        // address with its scheme swapped.
        let mut segments = path_segments(rest);
        if segments.first().is_some_and(|host| is_web_host(host)) {
            segments.remove(0);
        }
        segments
    } else if let Some(rest) = text.strip_prefix("spotify:") {
        rest.split(':').filter(|part| !part.is_empty()).collect()
    } else {
        let rest = text
            .strip_prefix("https://")
            .or_else(|| text.strip_prefix("http://"))?;
        let mut segments = path_segments(rest);
        if !segments.first().is_some_and(|host| is_web_host(host)) {
            return None;
        }
        segments.remove(0);
        segments
    };
    // Old playlist links carry the owner: spotify:user:NAME:playlist:ID.
    if segments.len() >= 4 && segments[0] == "user" && segments[2] == "playlist" {
        segments.drain(..2);
    }
    // The web address may start with a locale: open.spotify.com/intl-de/…
    if segments
        .first()
        .is_some_and(|first| first.starts_with("intl-"))
    {
        segments.remove(0);
    }
    let [kind, id, ..] = segments.as_slice() else {
        return None;
    };
    let kind = kind.to_ascii_lowercase();
    if !KINDS.contains(&kind.as_str()) || !is_id(id) {
        return None;
    }
    Some(format!("spotify:{kind}:{id}"))
}

/// Decodes a search link once. A path's `+` is literal, not a form-encoded
/// space. Canonical links encode the whole query so delimiters and Unicode
/// survive command-line, D-Bus, Apple Event and line-based socket delivery.
pub fn search_query(text: &str) -> Option<String> {
    let text = text.trim();
    let encoded = if text.starts_with("spotify:") && !text.starts_with("spotify://") {
        let (kind, query) = text.strip_prefix("spotify:")?.split_once(':')?;
        if !kind.eq_ignore_ascii_case("search") {
            return None;
        }
        query
    } else {
        let path = if let Some(rest) = text.strip_prefix("spotify://") {
            match rest.split_once('/') {
                Some((host, path)) if is_web_host(host) => path,
                _ => rest,
            }
        } else {
            let rest = text
                .strip_prefix("https://")
                .or_else(|| text.strip_prefix("http://"))?;
            let (host, path) = rest.split_once('/')?;
            if !is_web_host(host) {
                return None;
            }
            path
        };
        let path = &path[..path.find(['?', '#']).unwrap_or(path.len())];
        let path = if path.starts_with("intl-") {
            path.split_once('/')?.1
        } else {
            path
        };
        let (kind, query) = path.split_once('/').unwrap_or((path, ""));
        if !kind.eq_ignore_ascii_case("search") {
            return None;
        }
        query
    };
    let query = percent_encoding::percent_decode_str(encoded)
        .decode_utf8()
        .ok()?;
    (!query.chars().any(char::is_control)).then(|| query.into_owned())
}

/// The path of a web address split at slashes, its query and fragment
/// dropped, empty segments (a trailing slash) with them.
fn path_segments(rest: &str) -> Vec<&str> {
    let end = rest.find(['?', '#']).unwrap_or(rest.len());
    rest[..end]
        .split('/')
        .filter(|part| !part.is_empty())
        .collect()
}

fn is_web_host(host: &str) -> bool {
    matches!(
        host.to_ascii_lowercase().as_str(),
        "open.spotify.com" | "play.spotify.com"
    )
}

/// Spotify ids are base62; anything else on a link is not one, whatever
/// hands it over.
fn is_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prerelease_web_and_protocol_links_keep_the_prerelease_id() {
        for link in [
            "spotify:prerelease:0kRaNkRxpO16BjxJU0IQAL",
            "spotify://prerelease/0kRaNkRxpO16BjxJU0IQAL",
            "https://open.spotify.com/prerelease/0kRaNkRxpO16BjxJU0IQAL?si=ignored&go=1",
            "https://open.spotify.com/intl-ru/prerelease/0kRaNkRxpO16BjxJU0IQAL",
        ] {
            assert_eq!(
                parse(link).as_deref(),
                Some("spotify:prerelease:0kRaNkRxpO16BjxJU0IQAL")
            );
        }
        assert!(parse("https://open.spotify.com/prerelease/invalid-id").is_none());
    }

    /// Every shape Spotify's own apps and site hand out lands on the one
    /// URI the app navigates by.
    #[test]
    fn every_link_shape_becomes_the_one_uri() {
        // #given / #when / #then
        for (link, uri) in [
            (
                "spotify:track:4uLU6hMCjMI75M1A2tKUQC",
                "spotify:track:4uLU6hMCjMI75M1A2tKUQC",
            ),
            (
                "spotify:playlist:37i9dQZF1DXcBWIGoYBM5M",
                "spotify:playlist:37i9dQZF1DXcBWIGoYBM5M",
            ),
            (
                "spotify:user:carmine:playlist:37i9dQZF1DXcBWIGoYBM5M",
                "spotify:playlist:37i9dQZF1DXcBWIGoYBM5M",
            ),
            (
                "spotify://album/1DFixLWuPkv3KT3TnV35m3",
                "spotify:album:1DFixLWuPkv3KT3TnV35m3",
            ),
            (
                "spotify://open.spotify.com/artist/4Z8W4fKeB5YxbusRsdQVPb",
                "spotify:artist:4Z8W4fKeB5YxbusRsdQVPb",
            ),
            (
                "https://open.spotify.com/track/4uLU6hMCjMI75M1A2tKUQC",
                "spotify:track:4uLU6hMCjMI75M1A2tKUQC",
            ),
            (
                "https://open.spotify.com/track/4uLU6hMCjMI75M1A2tKUQC?si=abc123&nd=1",
                "spotify:track:4uLU6hMCjMI75M1A2tKUQC",
            ),
            (
                "https://open.spotify.com/intl-it/album/1DFixLWuPkv3KT3TnV35m3/",
                "spotify:album:1DFixLWuPkv3KT3TnV35m3",
            ),
            (
                "https://open.spotify.com/user/carmine/playlist/37i9dQZF1DXcBWIGoYBM5M",
                "spotify:playlist:37i9dQZF1DXcBWIGoYBM5M",
            ),
            (
                "http://play.spotify.com/show/4rOoJ6Egrf8K2IrywzwOMk",
                "spotify:show:4rOoJ6Egrf8K2IrywzwOMk",
            ),
            (
                "https://OPEN.SPOTIFY.COM/episode/512ojhOuo1ktJprKbVcKyQ#top",
                "spotify:episode:512ojhOuo1ktJprKbVcKyQ",
            ),
            (
                "  spotify:Artist:4Z8W4fKeB5YxbusRsdQVPb\n",
                "spotify:artist:4Z8W4fKeB5YxbusRsdQVPb",
            ),
        ] {
            assert_eq!(parse(link).as_deref(), Some(uri), "{link}");
        }
    }

    /// What is not a page here, or not Spotify's at all, is refused rather
    /// than guessed at.
    #[test]
    fn what_is_not_a_page_is_refused() {
        // #given / #when / #then
        for link in [
            "",
            "spotify:",
            "spotify:track",
            "spotify:track:",
            "spotify:user:carmine",
            "spotify:search:bad%0Aquery",
            "spotify:station:track:4uLU6hMCjMI75M1A2tKUQC",
            "spotify:local:Artist:Album:Song:180",
            "spotify:track:4uLU6hMCjMI75M1A2tKUQC/../etc",
            "spotify:track:a b",
            "https://example.com/track/4uLU6hMCjMI75M1A2tKUQC",
            "https://open.spotify.com/",
            "https://open.spotify.com/user/carmine",
            "https://open.spotify.com/intl-it/",
            "file:///etc/passwd",
            "4uLU6hMCjMI75M1A2tKUQC",
        ] {
            assert_eq!(parse(link), None, "{link:?}");
        }
        let long = format!("spotify:track:{}", "x".repeat(65));
        assert_eq!(parse(&long), None);
    }

    #[test]
    fn search_links_preserve_the_query_across_normalization_and_delivery() {
        for (link, query) in [
            (
                "https://open.spotify.com/search/here%20comes%20the%20sun",
                "here comes the sun",
            ),
            (
                "https://open.spotify.com/intl-de/search/artist%3ABj%C3%B6rk?si=share#top",
                "artist:Björk",
            ),
            (
                "spotify:search:artist:Radiohead year:1997",
                "artist:Radiohead year:1997",
            ),
            ("spotify://search/%E6%9D%B1%E4%BA%AC", "東京"),
            ("spotify://open.spotify.com/search/AC%2FDC", "AC/DC"),
            ("http://play.spotify.com/search/C%2B%2B+100%25", "C+++100%"),
            ("spotify:search:%2520", "%20"),
            ("https://open.spotify.com/search", ""),
            ("https://open.spotify.com/search/", ""),
            ("spotify:search:", ""),
        ] {
            assert_eq!(search_query(link).as_deref(), Some(query), "{link}");
            let canonical = parse(link).unwrap();
            assert_eq!(search_query(&canonical).as_deref(), Some(query));
            assert_eq!(parse(&canonical).as_ref(), Some(&canonical));
            assert!(canonical.is_ascii() && !canonical.contains(['\n', ' ']));
        }
        for invalid in [
            "https://example.com/search/song",
            "https://open.spotify.com.evil/search/song",
            "https://user@open.spotify.com/search/song",
            "file:///search/song",
            "spotify:search:bad%FFutf8",
            "https://open.spotify.com/search/line%0Abreak",
            "spotify:search:zero%00byte",
        ] {
            assert_eq!(parse(invalid), None, "{invalid}");
        }
    }
}
