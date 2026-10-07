---
title: Translating Spotizgeg
description: Help translate Spotizgeg and preview the work so far.
nav_order: 6
---

Spotizgeg follows your computer's language when it has a translation for it,
and uses English otherwise. **Settings → Appearance → Language** picks another
language, listed under its own name, and applies it at once; **System** follows
the computer again. This arrived in 0.10.0. Corrections from
fluent speakers are welcome.

Translations are stored in `.po` files, a common format supported by editors
such as Poedit and Weblate. They are included with Spotizgeg, so the app does
not contact an online translation service.

## Languages and coverage

| Language | Tag | Coverage |
| --- | --- | --- |
| English | `en` | Source language |
| Spanish | `es` | Complete |
| German | `de-DE` | Partial |
| Dutch | `nl` | Partial |
| Portuguese (Brazil) | `pt-BR` | Partial |
| Portuguese (Portugal) | `pt-PT` | Partial |
| French | `fr` | Partial |
| Swedish | `sv` | Partial |
| Polish | `pl` | Partial |
| Russian | `ru` | Partial |
| Italian | `it` | Partial |
| Japanese | `ja` | Partial |
| Chinese (Simplified) | `zh-Hans` | Partial |
| Chinese (Traditional) | `zh-Hant` | Partial |
| Turkish | `tr` | Complete |

The interface is marked for translation throughout: navigation, Home, Search,
Library and collection pages, menus, the player bar, Queue and Lyrics, dialogs,
keyboard shortcuts, sign-in, Settings and notifications, including tooltips
and screen-reader names. The partial catalogues translate the earlier pilot:
navigation, Library controls, the player bar, Queue and Lyrics. Everything they
do not translate yet appears in English. The tray menu, the macOS menu bar and
Dock menu, and the Windows taskbar buttons are still English in every language,
as are error details reported by Spotify, the network or the system.

A regional system language uses the closest catalogue: `es-MX` and `es-419`
use Spanish, `de-AT` uses German, `pt-BR` and a plain `pt` use Portuguese
(Brazil), other Portuguese regions use Portuguese (Portugal), `zh-CN` and
`zh-SG` use Simplified Chinese, and `zh-TW`, `zh-HK` and `zh-MO` use
Traditional Chinese. When the system lists several preferred languages, the
first one with a catalogue wins.

Song, album, artist and playlist names come from Spotify or their creators and
are kept as provided, as are lyric lines and failure details. Generated queue
playlist names translate the surrounding words while retaining the song title
or the date in `YYYY-MM-DD` form.

## Edit and preview

The repository's `assets/i18n/spotizgeg.pot` is the English source template.
Open the PO for your language, such as `assets/i18n/es.po`, in your translation editor. Edit `msgstr` values;
keep `msgid`, `msgid_plural`, `msgctxt`, and placeholders such as `{count}`, `{date}`,
`{track}` and `{error}` unchanged.
Translator comments explain the placeholders. Clear a fuzzy flag only after
reviewing the translation against its current English source.

Build and preview your changes in demo mode, which uses sample music data and
needs no Spotify account. `--demo-language` takes a tag from the table above
and overrides both the setting and the system language:

```sh
cargo run --features demo -- --demo --demo-language es
cargo run --features demo -- --demo --demo-language es --demo-show light --demo-size 760x620
cargo run --features demo -- --demo --demo-language de-DE --demo-show playing-next
cargo run --features demo -- --demo --demo-language ja --demo-show lyrics-follow
```

Check a narrow and a normal window, light and dark themes, keyboard navigation,
and screen-reader names. `--demo-shot PATH` saves the preview and exits. When automating screenshots, give the process
its own XDG config, data and state directories on Linux so framework window and
scroll state do not carry between captures.

Panel fixtures also include `queue-empty`, `queue-loading`, `queue-error`,
`recents-empty`, `recents-loading`, `recents-error`, `lyrics-empty`,
`lyrics-loading`, `lyrics-error`, `lyrics-instrumental` and `lyrics-no-playback`.
Combine a lyrics fixture with `lyrics-fullscreen` first to preview that state
in full screen, for example `--demo-show lyrics-fullscreen,lyrics-error`.

## Update the template and check catalogs

Maintainers mark source phrases with `gettext(locale, "English text")` and whole
counted phrases with `ngettext(locale, "Singular", "Plural", count)`. Use
`pgettext(locale, "context", "English text")` when the same English word has
different meanings. For example, `Follow` in the `lyrics` context follows the
current lyric line, so its translation can differ from following an artist.
Interpolate with named placeholders, such as `gettext(locale, "Added {name}")`
followed by `.replace("{name}", &name)`, and explain each one in a
`// Translators:` comment above the phrase. Add any
new source file to `assets/i18n/POTFILES`. With GNU gettext tools that support
Rust installed, run:

```sh
.github/scripts/update-translations.sh
.github/scripts/update-translations.sh --check
cargo test --locked --test localization
```

The update command extracts the template with `xgettext` and merges it into
existing PO files with `msgmerge`. The check command verifies the template and
uses `msgfmt` to check catalog syntax and marked format placeholders. The
localization tests also check that every translated phrase preserves its named
placeholders, including phrases filled by string replacement. Submit changed
PO files and the template, together with any required source changes. Generated
Rust catalogs stay in Cargo's build directory and are not committed.

Normal application builds need no external gettext tools. The build validates
the PO files and compiles their translations and plural expressions to Rust.
Missing, empty, fuzzy, or incomplete plural entries fall back to the full English
phrase, so a catalogue can be published before it is complete. The localization
test requires completeness only of catalogues that claim it (currently
Spanish); every catalogue must keep the placeholders of what it translates. Each locale's `Plural-Forms` header determines its plural choices;
languages are not restricted to two forms.

## Another language or a correction

Create another PO from the template using your editor's new-translation command,
or `msginit`. Set its language and plural rules and translate what you can.
A maintainer must also register the locale in the app, including how system
language tags map to it, and preview it before it becomes available. Adding a
PO alone does not add a language to the Settings list.

Use the [translation problem form](https://github.com/crmne/spotifast/issues/new?template=translation.yml)
for incorrect wording, missing translations or text that does not fit. Each
report gets its own issue. Include the language, version, affected control, and
the text you see; a suggested correction is welcome. The catalog headers link
to this form too.
