# Contributing to Spotifast

Spotifast is a native Spotify client. Changes should improve the
desktop app without adding a browser, fallback services, or another backend.

## Before opening an issue

Search open and closed issues first. For a bug, use the bug form and include
the requested log and exact steps to reproduce it. Reports without enough
information to investigate may be closed.

For a feature, explain the user problem. Discuss large changes in an issue
before writing code. Existing code does not guarantee that a feature fits the
project.

Some boundaries come from Spotify or from upstream libraries:

- Local playback requires Spotify Premium because librespot requires it.
- Spotify Lossless is not available through librespot. Spotifast will
  reconsider it if librespot gains lawful upstream support; proposals that
  depend on bypassing Spotify's DRM are out of scope.
- Spotify tracks must come from Spotify. Substituting audio from YouTube,
  Piped, `yt-dlp`, or another catalogue is out of scope.
- Spotifast will not embed a browser engine, add telemetry, or introduce a
  Spotifast-operated service.

[What Spotify Lets a Client Do](docs/_reference/what-spotify-allows.md)
lists what each of the three surfaces offers and the requests none of them
can serve, with the reason for each; a request in its last section is
answered from there and closed.

Duplicate, out-of-scope, or incomplete issues may be closed with a short
explanation.

A bug can be closed once its fix is on `main` and the relevant checks pass,
with the commit and release status stated. Reporter confirmation is welcome
but is not required for closure. Reopen the issue if it persists after updating.

## Automated triage

[Copilot Triage](https://github.com/crmne/copilot-triage) assesses new and
reopened issues and new discussions. It uses the report and latest five
comments plus previous bot replies, adds up to two labels, and may condense a
long new issue into one concise recap. Follow-up replies must add help: a
necessary question, supported answer, applicable policy, released fix, or useful
issue link. Clear duplicates can be closed after comparison; related reports
stay open. Maintainers handle uncertain decisions and removing obsolete labels.

Human follow-ups can trigger reassessment. The agent decides whether a reply
would help; repeated updates and thanks usually need none. Use `/triage` to
request reassessment, `/triage mute` to stop automatic replies, or maintainer-only
`/triage unmute` to resume them. You can also run **Issue assessment** from Actions
with the report kind and number. Preview is enabled by default; turn it off to
apply the result. The agent uses scoped read-only tools to investigate; only
conversation state and the CLI installation are cached. A party-popper reaction
marks a completed assessment,
including one that needed no reply; it does not promise acceptance or a fix.
Bot comments are skipped; configured error-monitoring bots can open reports.
Model failures stay in the job summary.

The `COPILOT_ISSUE_ASSESSMENT_ENABLED` repository variable controls the workflow.
Edit `.github/triage.yml` for labels, replies, source files, and response policy.
The shared action follows tested `v0` releases in `.github/workflows/issue-assessment.yml`; its
implementation and regression tests live in the Copilot Triage repository.

## Design principles

1. **Native and fast.** Startup time, idle work, memory use, and binary size
   are product features. Keep the UI thread free of network and disk waits.
2. **Focused.** Prefer a complete, coherent workflow over a collection of
   settings, modes, and speculative features.
3. **Honest integrations.** Use Spotify's Web API and librespot for what they
   support. Do not scrape, impersonate capabilities, bypass technical
   protections, or silently replace one service with another.
4. **Cross-platform by default.** Linux, macOS, and Windows are supported
   products. Platform-specific code must be isolated and the other targets
   must keep compiling.
5. **Small dependency surface.** Reuse the standard library and existing
   crates where practical. A new dependency needs a concrete benefit worth
   its build time, binary size, maintenance, and security cost.
6. **Visible failure, private data.** Errors should be actionable, rate limits
   should be respected, and credentials must never appear in logs. Network
   behaviour belongs in the documentation.

## Pull requests

Keep each pull request to one change. Explain why it belongs in Spotifast,
what changed, and how you tested it. Avoid unrelated formatting, refactors,
generated prose, and large mechanical rewrites.

`main` has a linear history. Outside pull requests are squash-merged into one
focused commit with contributor credit; merge commits are not accepted.
Maintainer work is committed directly on `main`, one topic per commit. Use
fast-forward-only pulls and rebase unpublished local commits when needed.
Do not rewrite published history without explicit maintainer approval.

The same rules apply to hand-written and AI-assisted changes. The author must
understand every line and answer review comments with specific reasoning.

Code changes should include tests for behaviour that can regress. UI changes
should include before/after screenshots or a short recording in the pull
request, not in the repository, and should use demo mode where possible.
User-visible behaviour, settings, files, or network access must be documented
in the same pull request.

Dependencies come from crates.io or from repositories the maintainer owns
under `github.com/crmne/`. A `git` dependency or `[patch]` entry may point only
at such a repository, and adding or changing one needs the maintainer's
explicit approval. Never point a dependency at your own fork or any other
source. If a dependency needs a fix, send it upstream and link it from the pull
request; the maintainer can carry it in a `crmne` fork until a release
includes it. CI fails when `Cargo.toml`, `Cargo.lock` or `.cargo/config.toml`
names any other git source or registry.

### Settings

Do not add a setting (a new field in `Settings`, a toggle, a slider, a choice
of options) without the maintainer's approval in the issue first. The answer is
usually no: prefer a good default that serves everyone, or a keyboard shortcut
or command for the people who want something else. A pull request that adds a
setting without that approval will be asked to remove it. CI fails when a pull
request adds a field to a `Settings` struct in `src/settings.rs`, until the
maintainer adds the `setting-approved` label.

### Visual reviews

Review evidence lives in the pull request, never in the repository. Attach
captures and recordings to the pull request description, or to a comment when
you update them, by dragging the files into GitHub's editor. Do not commit
screenshots, comparison pages or recordings, under `docs/assets/` or anywhere
else. CI fails when `docs/assets/` holds a file no page uses, or when an image,
video or HTML file is named for a review (review, compare, comparison, before,
after). Only images the project itself uses, such as a figure a documentation
page shows or an icon the app draws, belong in the repository.

Use actual before-and-after captures, from demo mode where possible. Match the
data, page, interaction state, zoom and window size so the difference shows the
proposed change. Identify the baseline and candidate, and report which
platforms actually produced the captures.

Lay the captures out in the description as a table with Before and After side
by side, for light and dark themes at a narrow and a normal window size. Show
relevant open menus, loading, error and empty states, and attach a short
recording when the change moves. Explain the visible change briefly and list
any remaining checks.

An interactive HTML comparison, with theme and size selectors and Before and
After buttons or a slider, helps with larger changes. Attach it to the pull
request as one self-contained `.html` file with the captures embedded, which
GitHub hands readers as a download to open in a browser, or publish it
elsewhere, for example with GitHub Pages from your fork, and link it from the
description. A gist does not work: GitHub shows its HTML as source.

When reviewing several changes, provide one index with the PR number and name,
a selector and Previous/Next controls, and a direct link to each comparison.
Load only the selected review, and publish the index outside the repository
too. The maintainer can approve by PR number and give exceptions or requested
adjustments in a normal message.

Visual approval covers the described appearance and interaction; integration
still requires the relevant checks. Retain approval across rebases that preserve
that scope. Implement explicitly requested adjustments and update the evidence;
ask again only if the resulting scope goes beyond what was approved or requested.

### Checks

Run the same checks CI runs before submitting:

```sh
python3 .github/scripts/check-dependency-sources.py
python3 .github/scripts/check-review-evidence.py
python3 .github/scripts/check-new-settings.py --base origin/main
python3 packaging/test-launchers.py
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets
cargo test --locked --all-targets --all-features
cargo test --locked --all-features --doc
RUSTDOCFLAGS='-D warnings' cargo doc --locked --all-features --no-deps
(cd docs && bundle exec jekyll build)
```

Linux needs the development packages listed under
[Build from source](https://spotifast.rocks/getting-started/#build-from-source); `nix develop`
provides the complete development environment. The command compatibility test
also needs `dbus-run-session`, to use a private bus instead of the desktop's.
MilkDrop builds libprojectM
from source, so every platform also needs CMake, a C++ compiler, and
libclang (on Windows, vcpkg with `glew:x64-windows-static` installed and
`VCPKG_INSTALLATION_ROOT` pointing at it); `--no-default-features` leaves
MilkDrop out and needs none of that. CI repeats the test suite on Linux,
macOS, and Windows. Passing CI is required, but does not replace review
for correctness, product fit, maintainability, or security.

Credential-storage changes also need a native store round trip. With the
desktop keyring unlocked, run
`cargo test --locked --lib credentials::tests::native_store_round_trip -- --ignored --exact`.
It uses temporary dummy grants and deletes them afterward. CI runs this check
on macOS and Windows; Linux requires an available Secret Service provider.
The ordinary test suite uses an isolated fake store and never reads a real
Spotify grant. Demo mode also skips credential restoration.

Flatpak state-persistence changes also need
`packaging/flatpak/test-state.sh`. It requires Flatpak, Ruby, and an installed
Platform runtime, and checks both manifests using disposable dummy state.
Pass a runtime and branch to use an existing installation, for example
`packaging/flatpak/test-state.sh org.kde.Platform 6.9`.

Translation changes also need `.github/scripts/update-translations.sh --check`,
using GNU gettext tools with Rust support. Run the script without `--check` when
translatable source strings change, and review any fuzzy or missing entries in
the updated PO files. Normal Cargo builds compile the catalogs without gettext
tools. See [Translating Spotifast](docs/_reference/translating.md) for language
coverage and the contributor workflow.

Documentation deployments take their canonical URL from the domain configured
in GitHub Pages. When changing domains, configure DNS and GitHub Pages before
redeploying; the previous hostname keeps working until that switch. Renamed
guides use `jekyll-redirect-from` to preserve their old URLs.

When changing `Cargo.lock` or `flake.nix`, also verify `nix build .#default`
on a Nix host or wait for the Nix CI job. A package-version-only lockfile
change can change the vendor hash. Releases must wait for all required CI
jobs on the version commit before the tag is pushed.

By contributing, you agree that your contribution is licensed under the
project's MIT License.
