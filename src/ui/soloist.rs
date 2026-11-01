//! Soloist is the first player offered after sign-in, with a built-in fallback.
//!
//! Setup is one card, one job at a time. After setup the app never comes back
//! here by itself: `banner` reports on the player over the working app, and
//! `settings` holds what stays true of it.
use super::settings::{RowText, filtered_note, filtered_row};
use crate::{
    app::App,
    i18n::gettext,
    model::Action,
    settings::SoloistOnboarding,
    soloist::{Failure, Status},
    theme,
};
use egui::{Align, CornerRadius, Frame, Layout, Margin, Stroke, Vec2};
use std::{borrow::Cow, time::Duration};

const GUIDE_URL: &str = "https://spotifast.rocks/soloist/";
const KEY_URL: &str = "https://developer.spotify.com/dashboard/soloist";
/// The card's width, as wide as the sign-in card with a little more to say.
const CARD: f32 = 460.0;
/// The height the card is placed for, so steps keep their top edge and grow
/// down rather than jumping around the middle of the window.
const CARD_HEIGHT: f32 = 480.0;
/// How long a step may wait in silence before the listener hears why.
const SLOW: Duration = Duration::from_secs(20);
/// How long pairing waits before the help offers itself.
const HELP_AFTER: Duration = Duration::from_secs(45);
const STEPS: usize = 3;
/// Set when a listener whose start failed chooses to enter another key.
const KEY_AGAIN: &str = "soloist-key-again";
const HELP_OPEN: &str = "soloist-help-open";

/// The one thing setup is doing now.
enum Stage {
    Welcome,
    Key(Option<Failure>),
    Starting,
    Downloading { received: u64, total: u64 },
    Pairing(String),
    Verifying(String),
    Ready,
    Failed(Failure),
}

impl Stage {
    fn of(app: &App, key_again: bool) -> Self {
        if app.soloist_onboarding == SoloistOnboarding::Welcome {
            return Self::Welcome;
        }
        match &app.soloist_status {
            Status::Off if !app.settings.soloist_enabled => Self::Key(None),
            Status::Off | Status::Starting => Self::Starting,
            Status::NeedsKey => Self::Key(None),
            Status::Downloading { received, total } => Self::Downloading {
                received: *received,
                total: *total,
            },
            Status::Failed(Failure::InvalidKey) => Self::Key(Some(Failure::InvalidKey)),
            Status::Failed(failure) if key_again => Self::Key(Some(failure.clone())),
            Status::Failed(_) if app.soloist_retry_at.is_some() => Self::Starting,
            Status::Failed(failure) => Self::Failed(failure.clone()),
            Status::Pairing { name, .. } => Self::Pairing(name.clone()),
            Status::Ready { .. } if app.soloist_device_id.is_some() => Self::Ready,
            Status::Ready { name, .. } => Self::Verifying(name.clone()),
        }
    }

    /// Which of the three steps this is; past the last one, all are done.
    fn step(&self) -> Option<usize> {
        match self {
            Self::Welcome => None,
            Self::Key(_) => Some(1),
            Self::Starting | Self::Downloading { .. } | Self::Failed(_) => Some(2),
            Self::Pairing(_) | Self::Verifying(_) => Some(3),
            Self::Ready => Some(STEPS + 1),
        }
    }
}

pub fn onboarding(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    egui::CentralPanel::default()
        .frame(Frame::new().fill(palette.window))
        .show(ui, |ui| {
            let rect = ui.max_rect();
            fastframe_titlebar::drag(
                ui,
                egui::Rect::from_min_size(
                    rect.min,
                    egui::vec2(
                        rect.width(),
                        theme::TOP_BAR_HEIGHT + theme::titlebar_inset(ui.ctx()),
                    ),
                ),
            );
            let top = super::blend(palette.window, palette.accent, 0.10);
            super::widgets::paint_vertical_gradient(ui, rect, top, palette.window);
            let width = CARD.min(rect.width() - 32.0);
            let margin: i8 = if width < CARD { 24 } else { 32 };
            egui::ScrollArea::vertical()
                .id_salt("soloist-onboarding")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add_space(((rect.height() - CARD_HEIGHT) / 2.0 - 12.0).max(40.0));
                    ui.vertical_centered(|ui| {
                        ui.set_max_width(width);
                        Frame::new()
                            .fill(palette.panel)
                            .stroke(Stroke::new(1.0, palette.outline))
                            .corner_radius(CornerRadius::same(theme::RADIUS + 8))
                            .inner_margin(Margin::same(margin))
                            .shadow(egui::epaint::Shadow {
                                offset: [0, 16],
                                blur: 48,
                                spread: 0,
                                color: palette.shadow,
                            })
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.with_layout(Layout::top_down(Align::Center), |ui| {
                                    ui.spacing_mut().item_spacing.y = 0.0;
                                    card(app, ui);
                                });
                            });
                        ui.add_space(18.0);
                        let locale = app.locale;
                        if app.soloist_setup_optional {
                            if theme::link(
                                ui,
                                gettext(locale, "Not now"),
                                theme::medium(13.0),
                                palette.text,
                            )
                            .clicked()
                            {
                                app.actions.push(Action::LeaveSoloistSetup);
                            }
                            ui.add_space(6.0);
                        }
                        if theme::link(
                            ui,
                            gettext(locale, "Use the built-in player"),
                            theme::regular(13.0),
                            palette.secondary,
                        )
                        .clicked()
                        {
                            app.actions.push(Action::UseBuiltInPlayer);
                        }
                    });
                    ui.add_space(32.0);
                });
        });
}

fn card(app: &mut App, ui: &mut egui::Ui) {
    let key_again_id = egui::Id::new(KEY_AGAIN);
    let key_again = ui.data(|data| data.get_temp::<bool>(key_again_id)) == Some(true);
    let stage = Stage::of(app, key_again);
    if let Some(step) = stage.step() {
        progress(app, ui, step);
        ui.add_space(22.0);
    }
    match stage {
        Stage::Welcome => welcome(app, ui),
        Stage::Key(failure) => key(app, ui, failure),
        Stage::Starting => starting(app, ui),
        Stage::Downloading { received, total } => downloading(app, ui, received, total),
        Stage::Pairing(name) => pairing(app, ui, &name),
        Stage::Verifying(name) => verifying(app, ui, &name),
        Stage::Ready => ready(app, ui),
        Stage::Failed(failure) => failed(app, ui, &failure),
    }
}

/// How far along setup is: a segment for each step, and the same in words.
fn progress(app: &App, ui: &mut egui::Ui, step: usize) {
    const SEGMENT: Vec2 = egui::vec2(40.0, 4.0);
    const GAP: f32 = 6.0;
    let palette = app.palette;
    let width = SEGMENT.x * STEPS as f32 + GAP * (STEPS as f32 - 1.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, SEGMENT.y), egui::Sense::hover());
    for index in 0..STEPS {
        let left = rect.left() + (SEGMENT.x + GAP) * index as f32;
        let segment = egui::Rect::from_min_size(egui::pos2(left, rect.top()), SEGMENT);
        let color = if index < step {
            palette.accent
        } else {
            palette.surface_active
        };
        ui.painter().rect_filled(segment, 2.0, color);
    }
    if step <= STEPS {
        ui.add_space(8.0);
        theme::text(
            ui,
            // Translators: {step} and {steps} are numbers, as in "Step 2 of 3".
            gettext(app.locale, "Step {step} of {steps}")
                .replace("{step}", &step.to_string())
                .replace("{steps}", &STEPS.to_string()),
            theme::medium(12.0),
            palette.secondary,
        );
    }
}

/// A round icon, tinted like the sign-in screen's step numbers.
fn badge(ui: &mut egui::Ui, palette: &theme::Palette, icon: theme::Icon, color: egui::Color32) {
    const SIZE: f32 = 52.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(SIZE), egui::Sense::hover());
    ui.painter().circle_filled(
        rect.center(),
        SIZE / 2.0,
        super::blend(palette.panel, color, 0.16),
    );
    theme::paint_icon(ui, icon, rect, SIZE * 0.5, color);
}

/// The step's title and its one sentence.
fn heading(ui: &mut egui::Ui, palette: &theme::Palette, title: &str, sentence: &str) {
    ui.add(
        egui::Label::new(
            egui::RichText::new(title)
                .font(theme::bold(23.0))
                .color(palette.text),
        )
        .wrap(),
    );
    ui.add_space(8.0);
    sentence_label(ui, palette, sentence);
}

fn sentence_label(ui: &mut egui::Ui, palette: &theme::Palette, sentence: &str) {
    ui.add(
        egui::Label::new(
            egui::RichText::new(sentence)
                .font(theme::regular(14.0))
                .color(palette.secondary),
        )
        .wrap(),
    );
}

/// Text that keeps its place in right-to-left and wrapping layouts.
fn wrapped(ui: &mut egui::Ui, label: &str, font: egui::FontId, color: egui::Color32) {
    let galley = crate::bidi::layout(
        ui.painter(),
        label,
        font,
        color,
        ui.available_width(),
        usize::MAX,
        None,
    );
    ui.add(egui::Label::new(galley));
}

/// Numbered steps read from the start of the line, under a centred heading.
fn left_aligned(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    ui.with_layout(Layout::top_down(Align::Min), |ui| {
        ui.spacing_mut().item_spacing.y = 8.0;
        add(ui);
    });
}

/// What is happening now, while nothing is asked of the listener.
fn activity(ui: &mut egui::Ui, palette: &theme::Palette, label: &str) {
    ui.horizontal(|ui| {
        let gap = 10.0;
        ui.spacing_mut().item_spacing.x = gap;
        let galley = crate::bidi::layout(
            ui.painter(),
            label,
            theme::medium(13.5),
            palette.text,
            (ui.available_width() - 18.0 - gap).max(0.0),
            1,
            Some(crate::bidi::ELLIPSIS),
        );
        ui.add_space((ui.available_width() - 18.0 - gap - galley.size().x).max(0.0) / 2.0);
        theme::spinner(ui, 18.0, palette.accent);
        ui.add(egui::Label::new(galley).selectable(false));
    });
}

/// The name to look for in Spotify's list, never ellipsized.
fn device_name(ui: &mut egui::Ui, palette: &theme::Palette, name: &str) {
    Frame::new()
        .fill(palette.surface)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(12, 9))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(18.0), egui::Sense::hover());
                theme::paint_icon(ui, theme::Icon::Headphones, rect, 18.0, palette.accent);
                wrapped(ui, name, theme::semibold(14.5), palette.text);
            });
        });
}

fn welcome(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let locale = app.locale;
    badge(ui, &palette, theme::Icon::Headphones, palette.accent);
    ui.add_space(16.0);
    heading(
        ui,
        &palette,
        &gettext(locale, "Listen with Spotify Soloist"),
        &gettext(
            locale,
            "Spotify's own player, running on this computer. Spotifast sets it up and keeps it updated.",
        ),
    );
    ui.add_space(20.0);
    left_aligned(ui, |ui| {
        for (number, text) in [
            gettext(locale, "Get your personal key from Spotify."),
            gettext(locale, "Spotifast downloads the player."),
            gettext(locale, "Choose it once in the Spotify app."),
        ]
        .iter()
        .enumerate()
        {
            super::login::step(ui, &palette, number + 1, text, |_| {});
        }
    });
    ui.add_space(16.0);
    if super::login::big_button(ui, app, &gettext(locale, "Set up Soloist")) {
        app.actions.push(Action::StartSoloistSetup);
    }
    ui.add_space(12.0);
    theme::text(
        ui,
        gettext(locale, "Requires Spotify Premium."),
        theme::regular(12.5),
        palette.secondary,
    );
    ui.add_space(6.0);
    guide_link(app, ui);
}

fn guide_link(app: &mut App, ui: &mut egui::Ui) {
    if theme::link(
        ui,
        gettext(app.locale, "Setup, privacy and limitations"),
        theme::regular(12.5),
        app.palette.secondary,
    )
    .clicked()
    {
        app.actions.push(Action::OpenUrl(GUIDE_URL.into()));
    }
}

fn key(app: &mut App, ui: &mut egui::Ui, failure: Option<Failure>) {
    let palette = app.palette;
    let locale = app.locale;
    heading(
        ui,
        &palette,
        &gettext(locale, "Get your Soloist key"),
        &gettext(
            locale,
            "Spotify gives each Premium account its own key. Spotifast keeps it in your system's credential store.",
        ),
    );
    ui.add_space(20.0);
    let mut typed = std::mem::take(&mut app.soloist_key);
    let mut submit = false;
    let valid = crate::soloist::Key {
        account: "typed".into(),
        value: typed.trim().into(),
    }
    .valid();
    left_aligned(ui, |ui| {
        super::login::step(
            ui,
            &palette,
            1,
            &gettext(locale, "Copy your key from Spotify's Soloist page."),
            |ui| {
                if theme::link(
                    ui,
                    "developer.spotify.com/dashboard/soloist",
                    theme::medium(13.0),
                    palette.accent,
                )
                .on_hover_text(KEY_URL)
                .clicked()
                {
                    app.actions.push(Action::OpenUrl(KEY_URL.into()));
                }
            },
        );
        super::login::step(ui, &palette, 2, &gettext(locale, "Paste it here."), |ui| {
            let response = Frame::new()
                .fill(palette.surface)
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::symmetric(12, 9))
                .show(ui, |ui| {
                    super::widgets::text_edit(
                        ui,
                        locale,
                        egui::TextEdit::singleline(&mut typed)
                            .id(egui::Id::new("soloist-key"))
                            .password(true)
                            .hint_text(
                                egui::RichText::new(gettext(locale, "Soloist key"))
                                    .color(palette.dim),
                            )
                            .font(theme::regular(14.0))
                            .frame(egui::Frame::NONE)
                            .desired_width(ui.available_width()),
                    )
                })
                .inner;
            submit = response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
            let message = if !typed.trim().is_empty() && !valid {
                Some(Failure::InvalidKey.message(locale))
            } else {
                failure.as_ref().map(|failure| failure.message(locale))
            };
            if let Some(message) = message {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(message)
                            .font(theme::regular(12.5))
                            .color(palette.danger),
                    )
                    .wrap(),
                );
            }
        });
    });
    app.soloist_key = typed;
    ui.add_space(14.0);
    let clicked = ui
        .add_enabled_ui(valid, |ui| {
            super::login::step_button(ui, app, &gettext(locale, "Download Soloist"))
        })
        .inner;
    if (clicked || submit) && valid {
        ui.data_mut(|data| data.remove_temp::<bool>(egui::Id::new(KEY_AGAIN)));
        app.actions.push(Action::SetSoloist(true));
    }
    ui.add_space(12.0);
    guide_link(app, ui);
}

fn starting(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let locale = app.locale;
    heading(
        ui,
        &palette,
        &gettext(locale, "Starting Soloist"),
        &gettext(locale, "Checking your key and starting Spotify's player."),
    );
    ui.add_space(24.0);
    theme::spinner(ui, 22.0, palette.accent);
    if app.soloist_status_at.elapsed() >= SLOW {
        ui.add_space(16.0);
        sentence_label(
            ui,
            &palette,
            &gettext(
                locale,
                "This is taking longer than usual. Spotify may be busy, so it's safe to wait.",
            ),
        );
        ui.add_space(12.0);
        if theme::pill_button(ui, &palette, &gettext(locale, "Try again"), false).clicked() {
            app.actions.push(Action::RetrySoloist);
        }
    }
}

fn downloading(app: &mut App, ui: &mut egui::Ui, received: u64, total: u64) {
    let palette = app.palette;
    let locale = app.locale;
    heading(
        ui,
        &palette,
        &gettext(locale, "Downloading Soloist"),
        &gettext(locale, "Directly from Spotify, to this computer."),
    );
    ui.add_space(24.0);
    let fraction = (total > 0).then(|| (received as f32 / total as f32).clamp(0.0, 1.0));
    let label = fraction.map(|fraction| {
        let megabytes = |bytes: u64| format!("{:.1}", bytes as f64 / 1_000_000.0);
        // Translators: {received} and {total} are numbers of megabytes.
        let size = gettext(locale, "{received} of {total} MB")
            .replace("{received}", &megabytes(received))
            .replace("{total}", &megabytes(total));
        format!("{size} • {}%", (fraction * 100.0).round() as u32)
    });
    progress_bar(ui, &palette, fraction, label.as_deref().unwrap_or_default());
    if let Some(label) = label {
        ui.add_space(10.0);
        theme::text(ui, label, theme::regular(12.5), palette.secondary);
    }
}

/// A thin bar in the accent colour; without a known size, a sliding segment.
pub(super) fn progress_bar(
    ui: &mut egui::Ui,
    palette: &theme::Palette,
    fraction: Option<f32>,
    label: &str,
) {
    let size = egui::vec2(ui.available_width(), 8.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::ProgressIndicator, ui.is_enabled(), label)
    });
    ui.painter().rect_filled(rect, 4.0, palette.surface_active);
    let fill = match fraction {
        Some(fraction) => {
            egui::Rect::from_min_size(rect.min, egui::vec2(rect.width() * fraction, rect.height()))
        }
        None => {
            ui.ctx().request_repaint();
            let width = rect.width() * 0.3;
            let phase = (ui.input(|input| input.time) * 0.5).fract() as f32;
            let left = rect.left() - width + (rect.width() + width) * phase;
            egui::Rect::from_min_max(
                egui::pos2(left.max(rect.left()), rect.top()),
                egui::pos2((left + width).min(rect.right()), rect.bottom()),
            )
        }
    };
    ui.painter().rect_filled(fill, 4.0, palette.accent);
}

fn pairing(app: &mut App, ui: &mut egui::Ui, name: &str) {
    let palette = app.palette;
    let locale = app.locale;
    heading(
        ui,
        &palette,
        &gettext(locale, "Choose Soloist in Spotify"),
        &gettext(locale, "You only do this once, on the same network."),
    );
    ui.add_space(20.0);
    left_aligned(ui, |ui| {
        super::login::step(
            ui,
            &palette,
            1,
            &gettext(locale, "Open the Spotify desktop app, on the same account."),
            |_| {},
        );
        super::login::step(
            ui,
            &palette,
            2,
            &gettext(
                locale,
                "Play a song. Soloist often only appears while one plays.",
            ),
            |_| {},
        );
        super::login::step(
            ui,
            &palette,
            3,
            &gettext(locale, "Open its list of devices and choose:"),
            |ui| device_name(ui, &palette, name),
        );
    });
    ui.add_space(14.0);
    activity(ui, &palette, &gettext(locale, "Waiting for Spotify…"));
    ui.add_space(12.0);
    let help_id = egui::Id::new(HELP_OPEN);
    let open = ui
        .data(|data| data.get_temp::<bool>(help_id))
        .unwrap_or(app.soloist_status_at.elapsed() >= HELP_AFTER);
    if theme::link(
        ui,
        gettext(locale, "Soloist isn't in the list"),
        theme::regular(13.0),
        palette.secondary,
    )
    .clicked()
    {
        ui.data_mut(|data| data.insert_temp(help_id, !open));
    }
    if open {
        ui.add_space(10.0);
        pairing_help(app, ui);
    }
}

/// What has worked for people whose Spotify did not list the player (#1142).
fn pairing_help(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let locale = app.locale;
    Frame::new()
        .fill(palette.surface)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.with_layout(Layout::top_down(Align::Min), |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                for tip in [
                    gettext(
                        locale,
                        "Phones and the web player may not list it until it has been chosen once. Start with the desktop app, on this computer.",
                    ),
                    gettext(
                        locale,
                        "A firewall, VPN or guest Wi-Fi can hide it. Allow Soloist on your local network, or pair once over your phone's hotspot.",
                    ),
                ] {
                    wrapped(ui, &tip, theme::regular(12.5), palette.secondary);
                }
                if theme::link(
                    ui,
                    gettext(locale, "More help in the guide"),
                    theme::medium(12.5),
                    palette.accent,
                )
                .clicked()
                {
                    app.actions.push(Action::OpenUrl(format!("{GUIDE_URL}#setup")));
                }
            });
        });
}

/// Soloist is signed in, and Spotify's account still has to list it.
fn verifying(app: &mut App, ui: &mut egui::Ui, name: &str) {
    let palette = app.palette;
    let locale = app.locale;
    heading(
        ui,
        &palette,
        &gettext(locale, "Almost there"),
        &gettext(
            locale,
            "Soloist is connected. Spotify is confirming it's on your account.",
        ),
    );
    ui.add_space(20.0);
    device_name(ui, &palette, name);
    ui.add_space(16.0);
    activity(ui, &palette, &gettext(locale, "Verifying Soloist…"));
    if app.soloist_status_at.elapsed() >= SLOW {
        ui.add_space(14.0);
        sentence_label(
            ui,
            &palette,
            &gettext(
                locale,
                "Spotify is slow to answer. Playing a song on Soloist from the Spotify app can help.",
            ),
        );
    }
}

fn ready(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let locale = app.locale;
    badge(ui, &palette, theme::Icon::Check, palette.accent);
    ui.add_space(16.0);
    heading(
        ui,
        &palette,
        &gettext(locale, "Ready to listen"),
        &gettext(locale, "Soloist will play your music on this computer."),
    );
    ui.add_space(24.0);
    if super::login::big_button(ui, app, &gettext(locale, "Start listening")) {
        app.actions.push(Action::FinishSoloistSetup);
    }
}

fn failed(app: &mut App, ui: &mut egui::Ui, failure: &Failure) {
    let palette = app.palette;
    let locale = app.locale;
    badge(ui, &palette, theme::Icon::CircleAlert, palette.danger);
    ui.add_space(16.0);
    heading(
        ui,
        &palette,
        &gettext(locale, "Soloist didn't start"),
        &failure.message(locale),
    );
    ui.add_space(24.0);
    if super::login::big_button(ui, app, &gettext(locale, "Try again")) {
        app.actions.push(Action::RetrySoloist);
    }
    ui.add_space(12.0);
    if theme::link(
        ui,
        gettext(locale, "Enter a different key"),
        theme::regular(13.0),
        palette.secondary,
    )
    .clicked()
    {
        ui.data_mut(|data| data.insert_temp(egui::Id::new(KEY_AGAIN), true));
    }
}

/// The player's state over the working app, for someone who finished setup.
/// Nothing here needs them unless it says so, and it goes when Soloist is back.
pub fn banner(app: &mut App, ui: &mut egui::Ui) {
    if !app.soloist_waiting() {
        return;
    }
    let locale = app.locale;
    let built_in = (
        gettext(locale, "Use the built-in player"),
        Action::UseBuiltInPlayer,
    );
    let connect = gettext(locale, "Connect in Spotify");
    let reconnecting = gettext(locale, "Reconnecting Soloist…");
    let browsing = gettext(
        locale,
        "Keep browsing. A song you choose now starts when it's back.",
    );
    let (title, text, busy, buttons): (_, Cow<'static, str>, _, _) = match &app.soloist_status {
        Status::Failed(failure) if app.soloist_retry_at.is_some() => {
            let text = format!("{} {browsing}", failure.message(locale));
            (reconnecting, text.into(), true, vec![])
        }
        Status::Failed(failure) => (
            gettext(locale, "Soloist isn't running"),
            failure.message(locale),
            false,
            vec![
                (gettext(locale, "Try again"), Action::RetrySoloist),
                built_in,
            ],
        ),
        Status::NeedsKey => (
            gettext(locale, "Soloist needs your key"),
            gettext(locale, "Spotifast can't find the key you saved."),
            false,
            vec![
                (gettext(locale, "Add your key"), Action::StartSoloistSetup),
                built_in,
            ],
        ),
        Status::Downloading { received, total } => {
            let percent = (*received * 100).checked_div(*total).unwrap_or(0);
            let source = gettext(locale, "Directly from Spotify, to this computer.");
            (
                gettext(locale, "Downloading Soloist"),
                format!("{source} {percent}%").into(),
                true,
                vec![],
            )
        }
        Status::Pairing { .. } if !app.soloist_recovering() => (
            gettext(locale, "Soloist needs to connect again"),
            gettext(
                locale,
                "Spotify signed it out on this computer. Choose it once more in Spotify.",
            ),
            false,
            vec![(connect, Action::StartSoloistSetup), built_in],
        ),
        Status::Ready { .. } if !app.soloist_recovering() => (
            gettext(locale, "Spotify hasn't listed Soloist yet"),
            gettext(
                locale,
                "Soloist is running. Playing a song on it from the Spotify app usually helps.",
            ),
            false,
            vec![(connect, Action::StartSoloistSetup), built_in],
        ),
        _ => (reconnecting, browsing, true, vec![]),
    };
    super::banner(app, ui, &title, &text, busy, buttons, None);
}

const OFFER: usize = 0;
const STATUS: usize = 1;
const VERSION: usize = 2;
const QUALITY: usize = 3;
const ACCESS: usize = 4;
const TURN_OFF: usize = 5;

/// What the Settings section's rows say, for drawing and for searching.
pub(super) fn rows(app: &App) -> [RowText<'static>; 6] {
    let locale = app.locale;
    let enabled = app.settings.soloist_enabled;
    let supported = crate::soloist::install::supported();
    [
        RowText::new(
            gettext(locale, "Listen with Spotify Soloist"),
            if supported {
                gettext(
                    locale,
                    "Spotify's own player, running on this computer. Requires Premium.",
                )
            } else {
                Failure::Unsupported.message(locale)
            },
        )
        .when(!enabled),
        RowText::new(gettext(locale, "Status"), status_text(app)).when(enabled),
        RowText::new(gettext(locale, "Version"), version_text(app)).when(
            enabled
                && matches!(
                    app.soloist_status,
                    Status::Ready { .. } | Status::Pairing { .. }
                ),
        ),
        RowText::new(
            gettext(locale, "Audio quality"),
            gettext(
                locale,
                "Spotify manages it. Soloist doesn't report the format it plays.",
            ),
        )
        .when(enabled),
        RowText::new(
            gettext(locale, "Spotify access"),
            gettext(
                locale,
                "Made for you, listening activity and Spotify lyrics.",
            ),
        )
        .when(enabled),
        RowText::new(
            gettext(locale, "Turn off Soloist"),
            gettext(
                locale,
                "Go back to the built-in player. Your key stays saved.",
            ),
        )
        .when(enabled),
    ]
}

fn status_text(app: &App) -> Cow<'static, str> {
    let locale = app.locale;
    match &app.soloist_status {
        Status::Ready { .. } if app.soloist_device_id.is_some() => {
            gettext(locale, "Ready on this computer.")
        }
        Status::Ready { .. } => gettext(locale, "Verifying Soloist…"),
        Status::Off | Status::Starting => gettext(locale, "Starting Soloist…"),
        Status::Downloading { received, total } => {
            let downloading = gettext(locale, "Downloading Soloist…");
            let percent = (*received * 100).checked_div(*total).unwrap_or(0);
            format!("{downloading} {percent}%").into()
        }
        Status::Pairing { .. } => gettext(locale, "Waiting for Spotify…"),
        Status::NeedsKey => gettext(locale, "Spotifast can't find the key you saved."),
        Status::Failed(failure) => failure.message(locale),
    }
}

fn version_text(app: &App) -> Cow<'static, str> {
    let locale = app.locale;
    let text: Cow<'static, str> = match &app.soloist_status {
        Status::Ready {
            version, expires, ..
        } => {
            let date = jiff::Timestamp::from_second((*expires).min(i64::MAX as u64) as i64)
                .ok()
                .map(|at| at.to_zoned(jiff::tz::TimeZone::system()).date().to_string())
                .unwrap_or_default();
            // Translators: {version} is Soloist's version; {date} is when this
            // build stops working, unless Spotifast has renewed it by then.
            gettext(
                locale,
                "{version}, good until {date}. Updates automatically.",
            )
            .replace("{version}", version)
            .replace("{date}", &date)
            .into()
        }
        Status::Pairing { version, .. } => version.clone().into(),
        _ => "".into(),
    };
    if app.soloist_update.is_some() {
        let update = gettext(
            locale,
            "Update ready. It will install when Soloist is inactive.",
        );
        format!("{text} {update}").into()
    } else {
        text
    }
}

/// The Settings section: one group of rows, and setup itself stays in setup.
/// An empty `needle` shows them all.
pub(super) fn settings(
    app: &mut App,
    ui: &mut egui::Ui,
    needle: &str,
    section: &str,
    rows: &[RowText<'static>; 6],
) {
    let palette = app.palette;
    let locale = app.locale;
    filtered_row(ui, &palette, needle, section, &rows[OFFER], |ui| {
        if crate::soloist::install::supported()
            && theme::pill_button(ui, &palette, &gettext(locale, "Set up Soloist"), true).clicked()
        {
            app.actions.push(Action::EnableSoloist);
        }
    });
    if matches!(app.soloist_status, Status::Ready { .. }) && app.soloist_device_id.is_some() {
        filtered_note(ui, &palette, needle, section, &rows[STATUS]);
    } else {
        filtered_row(ui, &palette, needle, section, &rows[STATUS], |ui| {
            status_control(app, ui);
        });
    }
    filtered_note(ui, &palette, needle, section, &rows[VERSION]);
    filtered_note(ui, &palette, needle, section, &rows[QUALITY]);
    filtered_row(ui, &palette, needle, section, &rows[ACCESS], |ui| {
        super::settings::spotify_access_control(app, ui);
    });
    filtered_row(ui, &palette, needle, section, &rows[TURN_OFF], |ui| {
        if theme::pill_button(ui, &palette, &gettext(locale, "Turn off"), false).clicked() {
            app.actions.push(Action::SetSoloist(false));
        }
    });
    if needle.is_empty() {
        if theme::link(
            ui,
            gettext(locale, "Setup, privacy and limitations"),
            theme::regular(13.0),
            palette.secondary,
        )
        .clicked()
        {
            app.actions.push(Action::OpenUrl(GUIDE_URL.into()));
        }
        ui.add_space(10.0);
    }
}

/// What the status row offers: the one action that helps, if any.
fn status_control(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let locale = app.locale;
    let action = match &app.soloist_status {
        Status::Failed(_) if app.soloist_retry_at.is_none() => {
            Some((gettext(locale, "Try again"), Action::RetrySoloist))
        }
        Status::NeedsKey => Some((gettext(locale, "Add your key"), Action::StartSoloistSetup)),
        Status::Pairing { .. } if !app.soloist_recovering() => Some((
            gettext(locale, "Connect in Spotify"),
            Action::StartSoloistSetup,
        )),
        _ => None,
    };
    match action {
        Some((label, action)) => {
            if theme::pill_button(ui, &palette, &label, true).clicked() {
                app.actions.push(action);
            }
        }
        None => {
            theme::spinner(ui, 16.0, palette.secondary);
        }
    }
}
