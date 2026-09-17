//! Bundled gettext pilot. English remains the production language while demo
//! mode exercises translated navigation, library, player and panel labels.

use std::borrow::Cow;
use tr::Translator;

include!(concat!(env!("OUT_DIR"), "/catalogs.rs"));

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Locale {
    #[default]
    #[value(name = "en", alias = "en-US")]
    English,
    #[value(name = "de-DE", alias = "de")]
    German,
    #[value(name = "es")]
    Spanish,
    #[value(name = "nl")]
    Dutch,
    #[value(name = "pt-BR")]
    PortugueseBrazil,
    #[value(name = "pt-PT")]
    PortuguesePortugal,
    #[value(name = "fr")]
    French,
    #[value(name = "sv")]
    Swedish,
    #[value(name = "pl")]
    Polish,
    #[value(name = "ru")]
    Russian,
    #[value(name = "it")]
    Italian,
    #[value(name = "ja")]
    Japanese,
    #[value(name = "zh-Hans")]
    ChineseSimplified,
    #[value(name = "zh-Hant")]
    ChineseTraditional,
}

impl Locale {
    fn translator(self) -> Option<&'static dyn Translator> {
        match self {
            Self::English => None,
            Self::German => Some(&de_de::Translator),
            Self::Spanish => Some(&es::Translator),
            Self::Dutch => Some(&nl::Translator),
            Self::PortugueseBrazil => Some(&pt_br::Translator),
            Self::PortuguesePortugal => Some(&pt_pt::Translator),
            Self::French => Some(&fr::Translator),
            Self::Swedish => Some(&sv::Translator),
            Self::Polish => Some(&pl::Translator),
            Self::Russian => Some(&ru::Translator),
            Self::Italian => Some(&it::Translator),
            Self::Japanese => Some(&ja::Translator),
            Self::ChineseSimplified => Some(&zh_hans::Translator),
            Self::ChineseTraditional => Some(&zh_hant::Translator),
        }
    }

    pub fn liked_song_count(self, count: u32) -> String {
        ngettext(
            self,
            // Translators: Keep {count} exactly as written. It becomes the number of liked songs.
            "Playlist • {count} song",
            "Playlist • {count} songs",
            count,
        )
        .replace("{count}", &count.to_string())
    }

    pub fn song_count(self, count: u32) -> String {
        ngettext(
            self,
            // Translators: Keep {count} exactly as written. It becomes a number of songs.
            "{count} song",
            "{count} songs",
            count,
        )
        .replace("{count}", &count.to_string())
    }

    pub fn playlist_count(self, count: u32) -> String {
        ngettext(
            self,
            // Translators: Keep {count} exactly as written. It becomes a number of playlists.
            "{count} playlist",
            "{count} playlists",
            count,
        )
        .replace("{count}", &count.to_string())
    }

    pub fn folder_playlist_count(self, count: u32) -> String {
        ngettext(
            self,
            // Translators: Keep {count} exactly as written. It becomes the number of playlists a folder holds.
            "Folder • {count} playlist",
            "Folder • {count} playlists",
            count,
        )
        .replace("{count}", &count.to_string())
    }

    pub fn folder_state_label(self, name: &str, collapsed: bool) -> String {
        // Translators: Keep {name} exactly as written. It becomes the folder name.
        let label = if collapsed {
            gettext(self, "{name}, folder, collapsed")
        } else {
            gettext(self, "{name}, folder, expanded")
        };
        label.replace("{name}", name)
    }
}

/// The English source is also the fallback for untranslated messages.
pub fn gettext(locale: Locale, source: &'static str) -> Cow<'static, str> {
    locale
        .translator()
        .map_or(Cow::Borrowed(source), |catalog| {
            catalog.translate(source, None)
        })
}

/// Translate a phrase whose meaning depends on its interface context.
pub fn pgettext(locale: Locale, context: &'static str, source: &'static str) -> Cow<'static, str> {
    locale
        .translator()
        .map_or(Cow::Borrowed(source), |catalog| {
            catalog.translate(source, Some(context))
        })
}

/// Select a whole translated phrase using the catalog's gettext plural rules.
pub fn ngettext(
    locale: Locale,
    singular: &'static str,
    plural: &'static str,
    count: u32,
) -> Cow<'static, str> {
    locale.translator().map_or(
        Cow::Borrowed(if count == 1 { singular } else { plural }),
        |catalog| catalog.ntranslate(count.into(), singular, plural, None),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_messages_use_the_english_source() {
        assert_eq!(gettext(Locale::German, "Home"), "Start");
        let missing = "Not translated yet";
        assert_eq!(gettext(Locale::German, missing), missing);
        assert_eq!(gettext(Locale::English, "Home"), "Home");
        assert_eq!(Locale::default(), Locale::English);
    }

    #[test]
    fn contextual_messages_do_not_leak_into_other_meanings() {
        let source = "Follow";
        let context = "lyrics";
        assert_eq!(pgettext(Locale::German, context, source), "Folgen");
        assert_eq!(pgettext(Locale::Japanese, context, source), "追従");
        assert_eq!(pgettext(Locale::English, context, source), source);
        assert_eq!(pgettext(Locale::German, "artist", source), source);
        assert_eq!(gettext(Locale::German, source), source);
    }

    #[test]
    fn zero_one_and_many_songs_have_complete_localized_labels() {
        for (count, english, german) in [
            (0, "Playlist • 0 songs", "Playlist • 0 Titel"),
            (1, "Playlist • 1 song", "Playlist • 1 Titel"),
            (2, "Playlist • 2 songs", "Playlist • 2 Titel"),
            (
                100_000,
                "Playlist • 100000 songs",
                "Playlist • 100000 Titel",
            ),
        ] {
            assert_eq!(Locale::English.liked_song_count(count), english);
            assert_eq!(Locale::German.liked_song_count(count), german);
        }
    }
    #[test]
    fn short_counts_are_localized_without_parsing_complete_phrases() {
        assert_eq!(Locale::German.song_count(2), "2 Titel");
        assert_eq!(Locale::Japanese.song_count(2), "2曲");
        assert_eq!(Locale::German.playlist_count(1), "1 Playlist");
        assert_eq!(Locale::German.playlist_count(2), "2 Playlists");
        assert_eq!(Locale::Polish.playlist_count(2), "2 playlisty");
        assert_eq!(Locale::Russian.playlist_count(5), "5 плейлистов");
        for (locale, count, expected) in [
            (Locale::English, 1, "Folder • 1 playlist"),
            (Locale::English, 2, "Folder • 2 playlists"),
            (Locale::German, 1, "Ordner • 1 Playlist"),
            (Locale::German, 2, "Ordner • 2 Playlists"),
            (Locale::Polish, 1, "Folder • 1 playlista"),
            (Locale::Polish, 2, "Folder • 2 playlisty"),
            (Locale::Polish, 5, "Folder • 5 playlist"),
            (Locale::Russian, 1, "Папка • 1 плейлист"),
            (Locale::Russian, 3, "Папка • 3 плейлиста"),
            (Locale::Russian, 5, "Папка • 5 плейлистов"),
            (Locale::Japanese, 1, "フォルダー • 1件のプレイリスト"),
            (Locale::Japanese, 4, "フォルダー • 4件のプレイリスト"),
        ] {
            assert_eq!(
                locale.folder_playlist_count(count),
                expected,
                "{locale:?} with {count}"
            );
        }
    }

    #[test]
    fn folder_state_labels_are_completely_localized_phrases() {
        assert_eq!(
            Locale::English.folder_state_label("Road trips", true),
            "Road trips, folder, collapsed"
        );
        assert_eq!(
            Locale::German.folder_state_label("Unterwegs", false),
            "Unterwegs, Ordner, ausgeklappt"
        );
    }

    #[test]
    fn locale_plural_rules_cover_european_and_asian_forms() {
        for (locale, count, expected) in [
            (Locale::Spanish, 1, "Playlist • 1 canción"),
            (Locale::Spanish, 2, "Playlist • 2 canciones"),
            (Locale::French, 0, "Playlist • 0 titre"),
            (Locale::French, 2, "Playlist • 2 titres"),
            (Locale::Italian, 1, "Playlist • 1 brano"),
            (Locale::Italian, 2, "Playlist • 2 brani"),
            (Locale::PortugueseBrazil, 0, "Playlist • 0 música"),
            (Locale::PortuguesePortugal, 0, "Playlist • 0 músicas"),
            (Locale::Dutch, 2, "Playlist • 2 nummers"),
            (Locale::Swedish, 2, "Spellista • 2 låtar"),
            (Locale::Polish, 1, "Playlista • 1 utwór"),
            (Locale::Polish, 2, "Playlista • 2 utwory"),
            (Locale::Polish, 5, "Playlista • 5 utworów"),
            (Locale::Polish, 21, "Playlista • 21 utworów"),
            (Locale::Polish, 22, "Playlista • 22 utwory"),
            (Locale::Russian, 1, "Плейлист • 1 трек"),
            (Locale::Russian, 11, "Плейлист • 11 треков"),
            (Locale::Russian, 21, "Плейлист • 21 трек"),
            (Locale::Russian, 22, "Плейлист • 22 трека"),
            (Locale::Russian, 112, "Плейлист • 112 треков"),
            (Locale::Japanese, 0, "プレイリスト • 0曲"),
            (Locale::Japanese, 2, "プレイリスト • 2曲"),
            (Locale::ChineseSimplified, 2, "歌单 • 2 首歌曲"),
            (Locale::ChineseTraditional, 2, "播放清單 • 2 首歌曲"),
        ] {
            assert_eq!(locale.liked_song_count(count), expected);
        }
    }
}
