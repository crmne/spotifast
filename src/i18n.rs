//! Bundled gettext catalogs. English is the source language and the fallback
//! for every message a catalog has not translated yet. The interface follows
//! the operating system's language unless Settings names another one.

pub use fastframe_i18n::{gettext, ngettext, pgettext};

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

impl fastframe_i18n::Locale for Locale {
    fn catalog(self) -> Option<&'static dyn fastframe_i18n::Translator> {
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
}

impl Locale {
    /// The tag this locale is named by on the command line and in
    /// `settings.json`. [`Self::from_tag`] reads each one back.
    pub fn tag(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::German => "de-DE",
            Self::Spanish => "es",
            Self::Dutch => "nl",
            Self::PortugueseBrazil => "pt-BR",
            Self::PortuguesePortugal => "pt-PT",
            Self::French => "fr",
            Self::Swedish => "sv",
            Self::Polish => "pl",
            Self::Russian => "ru",
            Self::Italian => "it",
            Self::Japanese => "ja",
            Self::ChineseSimplified => "zh-Hans",
            Self::ChineseTraditional => "zh-Hant",
        }
    }

    /// The locale a stored or typed tag names exactly, as [`Self::tag`]
    /// writes it or by one of its command-line aliases.
    pub fn from_tag(tag: &str) -> Option<Self> {
        <Self as clap::ValueEnum>::from_str(tag, true).ok()
    }

    /// The language's name in that language. A listener who has the app in a
    /// language they cannot read must still find their own in the picker, so
    /// these are never translated.
    pub fn native_name(self) -> &'static str {
        match self {
            Self::English => "English",
            Self::German => "Deutsch",
            Self::Spanish => "Español",
            Self::Dutch => "Nederlands",
            Self::PortugueseBrazil => "Português (Brasil)",
            Self::PortuguesePortugal => "Português (Portugal)",
            Self::French => "Français",
            Self::Swedish => "Svenska",
            Self::Polish => "Polski",
            Self::Russian => "Русский",
            Self::Italian => "Italiano",
            Self::Japanese => "日本語",
            Self::ChineseSimplified => "简体中文",
            Self::ChineseTraditional => "繁體中文",
        }
    }

    /// The language the operating system is read in, or English when it
    /// prefers only languages no catalog covers.
    ///
    /// The first preferred language with a catalog wins, so a desktop that
    /// lists Norwegian and then German gets German.
    pub fn from_system() -> Self {
        // Tests assert the English interface whatever the machine reads. A
        // test about another language sets it explicitly.
        if cfg!(test) {
            return Self::English;
        }
        fastframe_i18n::detect(Self::from_parsed_tag).unwrap_or_default()
    }

    /// The catalog closest to a language tag, BCP 47 (`pt-BR`, `zh-Hant-TW`)
    /// or POSIX (`es_UY.UTF-8`), or `None` when no catalog speaks it.
    pub fn from_language_tag(tag: &str) -> Option<Self> {
        fastframe_i18n::LanguageTag::parse(tag).and_then(|tag| Self::from_parsed_tag(&tag))
    }

    fn from_parsed_tag(tag: &fastframe_i18n::LanguageTag) -> Option<Self> {
        let region = tag.region.as_deref();
        Some(match tag.language.as_str() {
            "en" => Self::English,
            "de" => Self::German,
            "es" => Self::Spanish,
            "nl" => Self::Dutch,
            "fr" => Self::French,
            "sv" => Self::Swedish,
            "pl" => Self::Polish,
            "ru" => Self::Russian,
            "it" => Self::Italian,
            "ja" => Self::Japanese,
            // Portuguese outside Brazil follows the European standard. A bare
            // "pt" goes to Brazil, where most Portuguese speakers live.
            "pt" => match region {
                Some(
                    "pt" | "ao" | "cv" | "gw" | "mo" | "mz" | "st" | "tl" | "gq" | "ch" | "lu",
                ) => Self::PortuguesePortugal,
                _ => Self::PortugueseBrazil,
            },
            // A written script decides. Otherwise the region does, and
            // Simplified is the default, as in mainland China and Singapore.
            // Windows writes the legacy Chinese scripts as the regions CHS
            // and CHT.
            "zh" => match (tag.script.as_deref(), region) {
                (Some("hant"), _) => Self::ChineseTraditional,
                (Some("hans"), _) => Self::ChineseSimplified,
                (_, Some("tw" | "hk" | "mo" | "cht")) => Self::ChineseTraditional,
                _ => Self::ChineseSimplified,
            },
            _ => return None,
        })
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

/// Every bundled language, in the order the Settings picker lists them: by
/// their own names, as a reader of each would look for them.
pub const LOCALES: &[Locale] = &[
    Locale::German,
    Locale::English,
    Locale::Spanish,
    Locale::French,
    Locale::Italian,
    Locale::Dutch,
    Locale::Polish,
    Locale::PortugueseBrazil,
    Locale::PortuguesePortugal,
    Locale::Swedish,
    Locale::Russian,
    Locale::Japanese,
    Locale::ChineseSimplified,
    Locale::ChineseTraditional,
];

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
    fn system_language_tags_map_to_the_closest_catalog() {
        for (tag, expected) in [
            ("es_UY", Locale::Spanish),
            ("es-ES", Locale::Spanish),
            ("es_UY.UTF-8", Locale::Spanish),
            ("es-419", Locale::Spanish),
            ("de", Locale::German),
            ("de-AT", Locale::German),
            ("de_CH.UTF-8", Locale::German),
            ("en-GB", Locale::English),
            ("en_US.UTF-8", Locale::English),
            ("fr-CA", Locale::French),
            ("nl-BE", Locale::Dutch),
            ("sv_FI", Locale::Swedish),
            ("pl-PL", Locale::Polish),
            ("ru_UA.UTF-8", Locale::Russian),
            ("it-CH", Locale::Italian),
            ("ja-JP", Locale::Japanese),
            ("pt-BR", Locale::PortugueseBrazil),
            ("pt_BR.UTF-8", Locale::PortugueseBrazil),
            ("pt", Locale::PortugueseBrazil),
            ("pt-PT", Locale::PortuguesePortugal),
            ("pt_PT.UTF-8@euro", Locale::PortuguesePortugal),
            ("pt_MZ", Locale::PortuguesePortugal),
            ("pt-AO", Locale::PortuguesePortugal),
            ("zh", Locale::ChineseSimplified),
            ("zh-Hans", Locale::ChineseSimplified),
            ("zh-CN", Locale::ChineseSimplified),
            ("zh_CN.GB2312", Locale::ChineseSimplified),
            ("zh-SG", Locale::ChineseSimplified),
            ("zh-Hans-SG", Locale::ChineseSimplified),
            ("zh-Hans-HK", Locale::ChineseSimplified),
            ("zh-Hant", Locale::ChineseTraditional),
            ("zh-TW", Locale::ChineseTraditional),
            ("zh_TW.UTF-8", Locale::ChineseTraditional),
            ("zh-HK", Locale::ChineseTraditional),
            ("zh-MO", Locale::ChineseTraditional),
            ("zh-Hant-TW", Locale::ChineseTraditional),
            ("zh-CHT", Locale::ChineseTraditional),
            ("zh-CHS", Locale::ChineseSimplified),
        ] {
            assert_eq!(Locale::from_language_tag(tag), Some(expected), "{tag}");
        }
        for tag in ["nb-NO", "ko-KR", "ar", "C", "POSIX", "", "-", "und"] {
            assert_eq!(Locale::from_language_tag(tag), None, "{tag}");
        }
    }

    #[test]
    fn the_first_preferred_language_with_a_catalog_wins() {
        let tags = |list: &[&str]| list.iter().map(|tag| tag.to_string()).collect::<Vec<_>>();
        let preferred = |list: &[&str]| {
            fastframe_i18n::first_supported(tags(list), Locale::from_parsed_tag).unwrap_or_default()
        };
        assert_eq!(preferred(&["nb-NO", "de-DE", "en-US"]), Locale::German);
        assert_eq!(preferred(&["es-MX", "en-US"]), Locale::Spanish);
        assert_eq!(preferred(&["ko-KR"]), Locale::English);
        assert_eq!(preferred(&[]), Locale::English);
    }

    #[test]
    fn every_locale_round_trips_through_its_tag_and_is_listed_once() {
        for &locale in LOCALES {
            assert_eq!(Locale::from_tag(locale.tag()), Some(locale));
            assert_eq!(Locale::from_language_tag(locale.tag()), Some(locale));
            assert!(!locale.native_name().is_empty());
            assert_eq!(LOCALES.iter().filter(|other| **other == locale).count(), 1);
        }
        assert_eq!(
            LOCALES.len(),
            <Locale as clap::ValueEnum>::value_variants().len()
        );
        assert_eq!(Locale::from_tag("de"), Some(Locale::German));
        assert_eq!(Locale::from_tag("en-US"), Some(Locale::English));
        assert_eq!(Locale::from_tag("klingon"), None);
    }

    #[test]
    fn contextual_messages_do_not_leak_into_other_meanings() {
        let source = "Follow";
        let context = "lyrics";
        assert_eq!(pgettext(Locale::German, context, source), "Folgen");
        assert_eq!(pgettext(Locale::Japanese, context, source), "追従");
        assert_eq!(pgettext(Locale::English, context, source), source);
        assert_eq!(pgettext(Locale::German, "no such context", source), source);
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
        assert_eq!(Locale::German.song_count(2), "2 Songs");
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
            (Locale::Japanese, 1, "フォルダ • 1件のプレイリスト"),
            (Locale::Japanese, 4, "フォルダ • 4件のプレイリスト"),
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
