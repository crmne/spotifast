---
title: Translating Spotifast
description: Help translate Spotifast and preview the work so far.
nav_order: 6
---

**The regular app is currently in English.** Translations are being developed,
with early previews in 12 languages, including Portuguese and Chinese variants.
There is no language setting in the regular app yet. Corrections from fluent
speakers are welcome.

Translations are stored in `.po` files, a common format supported by editors
such as Poedit and Weblate. They are included with Spotifast, so the app does
not contact an online translation service.

## Pilot scope

Since 0.8.0, the pilot covers Home and Search navigation, Library
controls, filters, search hints, and the Liked Songs name and count. These
languages are available for preview in demo mode, which uses sample music
data and needs no Spotify account:

| Language | `--demo-language` |
| --- | --- |
| English | `en` |
| Spanish | `es` |
| German | `de-DE` |
| Dutch | `nl` |
| Portuguese (Brazil) | `pt-BR` |
| Portuguese (Portugal) | `pt-PT` |
| French | `fr` |
| Swedish | `sv` |
| Polish | `pl` |
| Russian | `ru` |
| Italian | `it` |
| Japanese | `ja` |
| Chinese (Simplified) | `zh-Hans` |
| Chinese (Traditional) | `zh-Hant` |

Current `main` also translates the player bar's empty state, tooltips and
screen-reader labels for playback, repeat, shuffle, Like, volume, device
selection, Queue and Lyrics controls. The Queue page and panel, Recent tab,
Lyrics panel and full-screen view, and shared loading/retry labels are also
translated on `main`. These additions are not in 0.8.0. They keep the existing
controls and keyboard actions. Other menus, pages and settings still need coverage.

Song, album, artist and playlist names come from Spotify or their creators and
are kept as provided, as are lyric lines and failure details. Generated queue
playlist names translate the surrounding words while retaining the song title
or the date in `YYYY-MM-DD` form. Interface text outside the pilot remains English.

## Edit and preview

The repository's `assets/i18n/fastpotify.pot` is the English source template.
Open the PO for your language, such as `assets/i18n/es.po`, in your translation editor. Edit `msgstr` values;
keep `msgid`, `msgid_plural`, `msgctxt`, and placeholders such as `{count}`, `{date}`,
`{track}` and `{error}` unchanged.
Translator comments explain the placeholders. Clear a fuzzy flag only after
reviewing the translation against its current English source.

Build and preview your changes with:

```sh
cargo run --features demo -- --demo --demo-language es
cargo run --features demo -- --demo --demo-language es --demo-show light --demo-size 760x620
cargo run --features demo -- --demo --demo-language de-DE --demo-show playing-next
cargo run --features demo -- --demo --demo-language ja --demo-show lyrics-follow
```

Check a narrow and a normal window, light and dark themes, keyboard navigation,
and screen-reader names. `--demo-shot PATH` saves the preview and exits. Demo
data needs no Spotify account. When automating screenshots, give the process
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
current lyric line, so its translation can differ from following an artist. Add any
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
phrase. Each locale's `Plural-Forms` header determines its plural choices;
languages are not restricted to two forms.

## Another language or a correction

Create another PO from the template using your editor's new-translation command,
or `msginit`. Set its language and plural rules and translate the pilot entries.
A maintainer must also register the locale in the app and preview it before it
becomes available. Adding a PO alone does not add a production language option.

Use the [translation problem form](https://github.com/crmne/spotifast/issues/new?template=translation.yml)
for incorrect wording, missing translations or text that does not fit. Each
report gets its own issue. Include the language, version, affected control, and
the text you see; a suggested correction is welcome. The catalog headers link
to this form too.
