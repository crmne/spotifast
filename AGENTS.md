# Spotifast agent guide

Follow `CONTRIBUTING.md`; it is the canonical product and contribution policy.
These instructions add implementation constraints for coding agents.

## Product boundaries

- Keep Spotifast a small native Spotify client. Do not add a browser engine,
  telemetry, a hosted backend, or alternate sources for Spotify audio.
- Playback capabilities come from librespot. Do not advertise or implement a
  capability merely because its name appears in a protobuf or enum. In
  particular, do not pursue Spotify Lossless or DRM circumvention unless
  lawful support first lands upstream.
- Do not broaden a task into adjacent features or a general refactor. Preserve
  existing user behaviour unless the task explicitly changes it.

## Architecture

- `src/ui/` draws views and emits `Action`s. Apply actions after drawing in
  `src/app.rs`; do not mutate application state from inside a borrowed view.
- Network and playback work belongs on the runtime in `src/backend.rs` or in
  the player engine in `src/player.rs`, never as blocking work on the UI
  thread.
- Keep platform integrations behind target-specific modules or `cfg` blocks.
  A fix for one platform must keep the other two targets compiling.
- Settings and state files must remain readable, backward compatible, and
  atomically written. Never log credentials or authorization responses.
- Prefer existing dependencies. Explain any new crate in `Cargo.toml` next to
  the dependency when the reason is not obvious.
- For dependency fixes, use a maintainer-owned fork pinned to a commit and
  contribute the fix upstream. Use the fork until a release includes the fix;
  do not copy dependency source into this repository.
- egui and winit come from forks shared with ZapFast, RekordFlash and TonePush
  (crmne/egui apps-0.36, crmne/winit apps-0.30); move all their crates to a
  new revision together. On Wayland, eframe from that fork paces frames by
  the compositor's frame callbacks instead of a vsync swap, so a hidden window
  cannot freeze the app (#266). Do not add a vsync decision of our own.
- The egui fork shapes right-to-left runs in their own direction but leaves
  them in logical order. Pass logical text to `crate::bidi`, which reorders
  the laid-out runs; never reorder the string before layout.

Read `docs/_reference/how-it-connects.md` before changing authentication,
Spotify requests, Connect, credential storage, or network behaviour. Read
`docs/_reference/queue.md` before touching the queue: its rules are the
contract, and the queue tests in `src/app.rs` enforce them. Read the
nearby module tests before changing a state machine or API fallback.

`docs/_reference/what-spotify-allows.md` lists what the Web API, the
librespot session, and librespot playback each offer, and the requests
none of them can serve (pins synchronised with Spotify, folder editing,
Smart Shuffle, lossless, local files, and more), each with its reason.
Before building or promising a Spotify-facing feature, and before answering
an issue that asks for one, find it there. A request in the last section is
answered with that reason and closed, not worked on; if the reason has
lapsed because librespot or the Web API gained the capability, update the
page in the same change.

The interface is optimistic, always. A control shows its result the
moment it is used: a double-clicked song is the playing song, Next pops
the queue's head, an added song has its row. The backend then makes it
true and Spotify's state catches up behind; an answer that still tells
the story from before the user's action is stale, so hold the shown
state and ask again rather than let the lagging answer undo what the
user just did. Nothing the user did may ever flicker away and come back.

Every visualiser, the spectrum analyser, the oscilloscope, and MilkDrop,
shows the signal post-equalizer and pre-volume: the EQ shapes what is
heard so the picture follows it, and the volume knob never moves the
picture. Zero volume still dances.

## Issue communication

- Write public replies for the reporter, not as an engineering investigation
  log. Keep them short, direct, and in plain language.
- A reply should move the issue forward: make the maintainer's decision, say
  that a fix is planned or in progress, or ask for one specific thing needed
  next. Include technical detail only when the reporter needs it to act.
- When a valid issue has a clear, bounded fix that can be implemented now,
  implement it instead of posting the proposed design in the issue. Do not use
  public comments as notes to yourself or as a substitute for doing the work.
- Close a bug once its fix is on `main` and the relevant checks pass. State
  which commit fixes it and whether it is released. Reporter confirmation is
  welcome, but is not a routine requirement for closure; reopen if the problem
  persists after updating. Keep an issue open when the fix is still uncertain
  or only part of the report has been addressed.
- Never post two maintainer comments in a row on the same issue or pull
  request. If nobody has replied since the last maintainer comment, edit that
  comment instead.
- Keep private investigation notes out of the public thread. Do not post a
  second comment merely to document more analysis.
- Never use em dashes. Use a full stop, comma, colon, or parentheses instead.

## Interface review

- Distinguish an internal UI refactor from an interface redesign. Moving
  navigation or controls, regrouping menus, changing the application shell,
  window chrome, panel ownership or sizing, responsive breakpoints, spacing,
  or visual hierarchy is a redesign even when behavior still works.
- Call out every user-visible interface change at the top of a pull request
  review. Correct code and green CI do not make a redesign merge-ready.
- Require explicit maintainer approval of the visual scope before merging an
  interface redesign. Conditional approval to assess code quality is not
  approval of changed appearance or interaction.
- Inspect before-and-after evidence at representative window sizes and in both
  light and dark themes. If that evidence is missing, request it.
- Use the HTML comparison format in `CONTRIBUTING.md` under "Visual reviews":
  matching captures, theme and size selectors, Before/After controls, and
  relevant interaction states. For a batch, provide one index with PR numbers,
  a selector, Previous/Next controls and links to individual comparisons.
- Record approval and requested adjustments by PR number in the triage ledger.
  Keep visual approval separate from outstanding implementation or test gates.
  Do not ask again for unchanged approved scope after a rebase. A concrete
  requested adjustment is authorization to make that adjustment and update its
  evidence; ask again only for scope beyond the approval or request.

## Branches

Work on `main`. Commit there directly, one topic per commit, each
compiling and passing the checks on its own. Feature branches and pull
requests are for outside contributors; the maintainer's own work, and
work done with the maintainer, does not go through them.

Keep `main` linear. Squash outside pull requests into one focused commit,
preserving contributor credit. Never create or push merge commits, including
local `git merge --no-ff` commits that bypass GitHub's squash-only setting.
When updating a local checkout, use fast-forward-only pulls; rebase unpublished
local commits if needed. Before pushing, verify that the commits being added
contain no merge commits. Rewriting published history requires explicit
maintainer approval and an exact force-with-lease guard; keep a recovery ref.

## Disk use

Builds go through [mbx](https://mr-boxington.jdx.dev), enabled for mise users
by `mise.toml` (run `mise trust` once in each new checkout or worktree, or
mise refuses to run `cargo` there). It keeps compiled work in one shared
store, places each checkout's `target/` under a disk budget, and collects old
outputs on its own. Plain `cargo` still works for contributors who do not use
mise or mbx.

- Give each worktree and each parallel agent its own target directory. A
  worktree's own `target/` is enough, and mbx manages it; a second build in
  the same checkout uses `CARGO_TARGET_DIR=target/<name>`, which stays inside
  the managed target. Never point builds at a shared target directory: Cargo's
  lock serializes them, one worktree's test run can execute another's binary,
  and the store already shares compiled outputs.
- Never vary `codegen-units` or other compiler flags per agent. Each variant
  is a separate cache entry and fills the disk.
- Do not `cargo clean` to save space. `mbx gc --dry-run` previews collection
  and `mbx gc` runs it now; `mbx cache stats` shows what is held.
- When a build is colder than expected, `mbx explain --last` says what missed
  the cache and why.
- Never put build output or large scratch files in `/tmp`. It is a small
  in-memory filesystem with a per-user quota, and filling it breaks every
  shell on the machine.
- Delete one-off QA, packaging, and release-validation directories (under
  `.cache/` or `~/.cache/`) once their result is recorded.

## Definition of done

- Add focused regression tests for changed behaviour. Use the `demo` feature
  for deterministic UI coverage and screenshots.
- Update the README and docs when user-visible behaviour, settings, files, or
  network access changes.
- Run the full checks from `CONTRIBUTING.md`. Do not weaken a lint, delete a
  test, or add an `allow` merely to make CI green without explaining why the
  underlying rule does not apply.
- Report platform coverage honestly. Do not claim a platform was tested when
  it was only compiled or reasoned about.

## Releases

A release is not the tag alone. Do these in order:

1. Change the `Cargo.toml` version, add the matching release to the Flatpak
   metainfo, and update the lockfile with a build.
   Refresh the `flake.nix` vendor hash when the lockfile changes, even when
   only the package version changed. Verify `nix build .#default` locally or
   in CI. Wait for every required CI job on the release commit before tagging.
   Commit and push this before the tag so the binaries report the right
   version. Include written notes in `packaging/release-notes/vVERSION.md`
   so the release publishes the real description immediately.
2. Push the `v*` tag, which triggers the release workflow. Wait for every
   required artifact and `checksums.txt`, then verify the published written
   notes, screenshot and download links. Never publish generated placeholder notes.
3. A prerelease stops here. Keep the stable version current on the website,
   Homebrew, and AUR. The prerelease remains available from GitHub's releases
   page.
4. For a stable release, only after the GitHub release exists, update
   `docs/_config.yml` `spotifast_version` and
   `docs/_data/versions.yml`. The selector carries only the latest stable
   version: replace its version entry, make it `current`, and point it at
   `/download/`. Do not retain older version entries; they remain available
   through the Changelog link. Never make the download page point at files
   that do not exist yet.
5. Update the Homebrew cask in the maintainer's tap and the AUR package from
   the release's `checksums.txt`. The packaging workflow handles configured
   destinations when `PUBLISH_HOMEBREW` and `PUBLISH_AUR` are enabled. Otherwise
   use the in-repository packaging CLI to prepare, review and publish them;
   see `PACKAGING.md`. Native package validation remains required.

Before writing release notes, read the previous two stable releases and match
their style. Start with a short plain-language summary, use `New` and `Fixed`
sections as applicable, lead each item with a bold user-facing result, credit
contributors and reporters with the relevant issue or pull request numbers,
include a `Thanks` section, and end with the full changelog link. Do not leave
the generated notes in place or introduce a different section scheme for
ordinary improvements.

Skipping an applicable step ships a release that lies somewhere; the dropdown
was forgotten once already.
