//! Window layout: panels, overlays, keyboard shortcuts.

use std::sync::Arc;

pub mod artist;
pub mod collection;
pub(crate) mod devices;
mod dialogs;
pub mod flight;
pub mod home;
mod keys;
pub mod library;
pub mod login;
mod lyrics;
pub mod player_bar;
mod prerelease;
pub mod queue;
pub mod radio;
pub mod search;
pub mod settings;
pub mod show;
pub mod sidebar;
pub mod topbar;
mod update;
pub mod widgets;
pub mod winamp;

use egui::{Align2, Color32, Context, CornerRadius, Frame, Id, Margin, Rect, Stroke, vec2};

use crate::api::models::pick_image;
use crate::app::App;
use crate::backend::AuthStatus;
use crate::model::{Action, Loadable, Page, ToastKind};
use crate::theme::{self, Icon};

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    let ctx = &ctx;
    keys::handle(app, ctx);
    for path in winamp::dropped_skins(ctx) {
        app.actions.push(Action::InstallSkin(path));
    }
    let signed_in = app.is_connected() && app.user.is_some();
    let connecting = matches!(app.auth, AuthStatus::Connecting | AuthStatus::Starting)
        || (app.is_connected() && app.user.is_none());
    if !signed_in {
        player_bar::end_tint_session(ctx);
        forget_glow(ctx);
        login::show(app, ui, connecting);
        update::show(app, ctx);
        toasts(app, ctx, 20.0);
        caption_buttons(ui, &app.palette, app.locale);
        fastframe_titlebar::resize(ui);
        return;
    }
    if titlebar_spans_window(cfg!(target_os = "macos"), fastframe_titlebar::enabled()) {
        fastframe_titlebar::drag(
            ui,
            Rect::from_min_size(
                ui.max_rect().min,
                vec2(
                    ui.max_rect().width(),
                    theme::top_bar_height() + theme::titlebar_inset(ui.ctx()),
                ),
            ),
        );
    }
    player_bar::show(app, ui);
    if app.lyrics_fullscreen.is_some() {
        lyrics::fullscreen(app, ui);
    } else {
        if app.settings.sidebar_visible {
            sidebar::show(app, ui);
        }
        if app.show_queue_panel {
            queue::side_panel(app, ui);
        }
        if app.show_lyrics_panel {
            lyrics::side_panel(app, ui);
        }
        central(app, ui);
        flight::show(app, ctx);
        keep_room_for_panels(app, ctx);
    }
    devices::popup(app, ctx);
    dialogs::show(app, ctx);
    update::show(app, ctx);
    widgets::drag_ghost(ctx, &app.palette, app.locale);
    toasts(app, ctx, theme::PLAYER_BAR_HEIGHT + 16.0);
    caption_buttons(ui, &app.palette, app.locale);
    fastframe_titlebar::resize(ui);
}

/// The main window's narrowest width with these panels open: their least
/// widths and the page's.
fn main_min_width(page: f32, sidebar: bool, right_panel: bool) -> f32 {
    let sidebar = if sidebar { SIDEBAR_MIN_WIDTH } else { 0.0 };
    let right = if right_panel {
        theme::SIDE_PANEL_MIN_WIDTH
    } else {
        0.0
    };
    (sidebar + right + page).max(crate::window::MAIN_MIN_SIZE[0])
}

/// The sidebar's narrowest width.
pub(crate) const SIDEBAR_MIN_WIDTH: f32 = 210.0;

/// Raise the window's minimum width while the Queue or Lyrics panel is open,
/// so even at its narrowest the page beside the panels keeps the room its
/// top bar needs (#624), and lower it again once they close. Window managers
/// that tile ignore the minimum; the panels still give way there.
fn keep_room_for_panels(app: &App, ctx: &Context) {
    if crate::window::fixed_size() {
        return;
    }
    let width = main_min_width(
        topbar::least_width(ctx),
        app.settings.sidebar_visible,
        app.show_queue_panel || app.show_lyrics_panel,
    )
    .round();
    let id = Id::new("main-min-width");
    let sent = ctx.data(|data| data.get_temp::<f32>(id));
    // The window starts at MAIN_MIN_SIZE, so there is nothing to send until
    // the panels need more.
    if sent.unwrap_or(crate::window::MAIN_MIN_SIZE[0]) != width {
        ctx.data_mut(|data| data.insert_temp(id, width));
        ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(vec2(
            width,
            crate::window::MAIN_MIN_SIZE[1],
        )));
    }
}

/// How a resizable side panel fits beside the page.
pub(crate) struct PanelFit {
    /// The widths the panel may take this frame.
    pub range: std::ops::RangeInclusive<f32>,
    /// Whether the room the page needs holds the panel under the width the
    /// person chose for it.
    pub yielding: bool,
}

/// Fit a side panel between `range` and the page beside it. The panel gives
/// up width, down to its minimum, so the page keeps `room_for_page`, the
/// least its top bar needs (#624). The width chosen for the panel stays in
/// the settings and comes back once the window widens again.
///
/// `room` is what the panel and everything after it share.
pub(crate) fn yielding_panel(
    ctx: &Context,
    id: &str,
    range: std::ops::RangeInclusive<f32>,
    chosen: f32,
    room: f32,
) -> PanelFit {
    let (min, max) = range.into_inner();
    let max = max.min(room).max(min);
    let width = chosen.clamp(min, max);
    // egui remembers the width the panel last drew at, a clamped one too.
    // Hand it back the chosen width whenever there is room for more.
    let id = Id::new(id);
    if let Some(mut state) = egui::containers::panel::PanelState::load(ctx, id)
        && state.outer_rect.width() + 0.5 < width
    {
        state.outer_rect.set_width(width);
        ctx.data_mut(|data| data.insert_persisted(id, state));
    }
    PanelFit {
        range: min..=max,
        yielding: max < chosen - 0.5,
    }
}

/// Whether a panel's new width is one the person chose: it moved while not
/// held back by the page, or by their own drag.
pub(crate) fn panel_width_chosen(ctx: &Context, id: &str, fit: &PanelFit) -> bool {
    !fit.yielding || ctx.is_being_dragged(Id::new(id).with("__resize"))
}

/// Keeps the most recent loading preview of each metadata type available to
/// the loaded hero as an artwork fallback. The fixed typed slot bounds this to
/// one playlist, album, artist, and show instead of scanning known metadata on
/// every loaded frame.
fn loading_preview<T>(
    ctx: &Context,
    key: &str,
    details: &Loadable<T>,
    known: impl FnOnce() -> Option<T>,
) -> Option<Arc<T>>
where
    T: Send + Sync + 'static,
{
    let memory_id = Id::new("collection-loading-preview");
    let key = Id::new(key);
    if details.get().is_some() {
        ctx.data(|data| data.get_temp::<(Id, Arc<T>)>(memory_id))
            .filter(|(stored_key, _)| *stored_key == key)
            .map(|(_, preview)| preview)
    } else {
        let preview = known().map(Arc::new);
        ctx.data_mut(|data| match &preview {
            Some(preview) => {
                data.insert_temp(memory_id, (key, Arc::clone(preview)));
            }
            None => data.remove::<(Id, Arc<T>)>(memory_id),
        });
        preview
    }
}

/// The colour a page's header glows with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum PageTint {
    Color(Color32),
    None,
    /// The page's cover is still being read for its colour: the glow holds
    /// what it shows rather than flash the song's colour first.
    Pending,
}

pub(crate) fn page_tint(app: &mut App) -> PageTint {
    let page = app.page().clone();
    let image = match &page {
        Page::Playlist(id) => app
            .playlist_pages
            .get(id)
            .and_then(|page| page.playlist.get())
            .or_else(|| app.known_playlist(id))
            .and_then(|playlist| pick_image(&playlist.images, 64))
            .map(str::to_string),
        Page::Album(id) => app
            .album_pages
            .get(id)
            .and_then(|page| page.album.get())
            .or_else(|| app.known_album(id))
            .and_then(|album| pick_image(&album.images, 64))
            .map(str::to_string),
        Page::Artist(id) => app
            .artist_pages
            .get(id)
            .and_then(|page| page.artist.get())
            .or_else(|| app.known_artist(id))
            .and_then(|artist| pick_image(&artist.images, 64))
            .map(str::to_string),
        Page::Show(id) => app
            .show_pages
            .get(id)
            .and_then(|page| page.show.get())
            .or_else(|| app.known_show(id))
            .and_then(|show| pick_image(&show.images, 64))
            .map(str::to_string),
        Page::Radio(seed) => pick_image(&app.radio_images(seed), 64).map(str::to_string),
        Page::LikedSongs => {
            return if app.settings.accent_from_art {
                PageTint::Color(Color32::from_rgb(0x50, 0x38, 0xc8))
            } else {
                PageTint::None
            };
        }
        _ => None,
    };
    if !app.settings.accent_from_art && image.is_some() {
        return PageTint::None;
    }
    match image {
        Some(url) => match app.tint_for(Some(&url)) {
            Some(color) => PageTint::Color(color),
            None if app.tint_missing(&url) => app.song_tint(),
            // While it is read, a cover flying into the page brings the
            // colour of the artwork it shows, already at hand, so the glow
            // rises with the flight rather than after it.
            None => app
                .cover_flight
                .as_ref()
                .filter(|flight| flight.page == page)
                .map(|flight| flight.origin.url.clone())
                .and_then(|flown| app.tint_for(Some(&flown)))
                .map_or(PageTint::Pending, PageTint::Color),
        },
        None => app.song_tint(),
    }
}

/// How quickly the glow follows a new colour or strength: most of the way
/// in about half a second, settled in about four fifths of one.
const GLOW_SECONDS: f32 = 0.16;

/// Where the header's glow as last painted is kept.
const GLOW_ID: &str = "spotifast-page-glow";

/// The header's glow as shown: its colour, unmultiplied, and how strongly
/// it shows over the window.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Glow {
    rgb: [f32; 3],
    strength: f32,
}

impl Glow {
    /// No glow at all.
    const DARK: Self = Self {
        rgb: [0.0; 3],
        strength: 0.0,
    };

    /// A glow of `color` at `strength`.
    fn of(color: Color32, strength: f32) -> Self {
        let [r, g, b, _] = color.to_srgba_unmultiplied();
        Self {
            rgb: [r, g, b].map(f32::from),
            strength,
        }
    }

    /// The colour as painted, rounded to whole levels.
    fn color(self) -> Color32 {
        let [r, g, b] = self
            .rgb
            .map(|channel| channel.round().clamp(0.0, 255.0) as u8);
        Color32::from_rgb(r, g, b)
    }

    /// `self` moved `elapsed` seconds towards `target`, easing out. A glow
    /// that has faded away takes the new colour at once, so it rises in that
    /// colour rather than through the old one. Less motion shows the end.
    fn toward(self, target: Self, elapsed: f32, still: bool) -> Self {
        if still {
            return target;
        }
        let rgb = if self.strength < 0.01 {
            target.rgb
        } else {
            self.rgb
        };
        let step = 1.0 - (-elapsed / GLOW_SECONDS).exp();
        let near = |from: f32, to: f32, close: f32| {
            let value = from + (to - from) * step;
            if (to - value).abs() < close {
                to
            } else {
                value
            }
        };
        // It settles once what is left would change the painted colour by
        // a level or so, rather than easing on unseen.
        Self {
            rgb: [0, 1, 2].map(|channel| near(rgb[channel], target.rgb[channel], 1.5)),
            strength: near(self.strength, target.strength, 0.006),
        }
    }
}

/// The glow as last painted, kept between frames.
#[derive(Clone, Copy)]
struct Shown {
    glow: Glow,
    /// Where it was heading, which a page still reading its colour keeps
    /// heading to.
    target: Glow,
    /// Whether the theme was dark.
    dark: bool,
    /// The palette's own wash it settled on without art.
    rest: Option<Glow>,
    at: f64,
}

/// The glow to paint this frame for `tint` at `strength`, easing from the
/// one painted last, and whether it is still moving. A page without a
/// colour settles on `rest`, the palette's own wash, or fades out.
fn glow(
    ctx: &Context,
    tint: PageTint,
    strength: f32,
    rest: Option<Glow>,
    still: bool,
) -> (Glow, bool) {
    let id = egui::Id::new(GLOW_ID);
    let now = ctx.input(|input| input.time);
    let dark = ctx.theme() == egui::Theme::Dark;
    let shown = ctx.data(|data| data.get_temp::<Shown>(id));
    // The palette's wash is there from the first frame, like its window
    // colour; only a cover's colour rises in.
    let current = shown.map_or(rest.unwrap_or(Glow::DARK), |shown| shown.glow);
    let target = match tint {
        PageTint::Color(color) => Glow::of(color, strength),
        PageTint::None => rest.unwrap_or(Glow {
            strength: 0.0,
            ..current
        }),
        PageTint::Pending => shown.map_or(current, |shown| shown.target),
    };
    // A new theme brings its own tint of the same colour under its reveal,
    // so the glow takes it at once rather than crossing over from the old;
    // so does a new palette, dark or light alike, whose wash has the old
    // window's colour in it.
    let still = still || shown.is_some_and(|shown| shown.dark != dark || shown.rest != rest);
    let elapsed = shown.map_or(0.0, |shown| (now - shown.at).clamp(0.0, 1.0 / 30.0) as f32);
    let glow = current.toward(target, elapsed, still);
    ctx.data_mut(|data| {
        data.insert_temp(
            id,
            Shown {
                glow,
                target,
                dark,
                rest,
                at: now,
            },
        )
    });
    (glow, glow != target)
}

/// Forget the glow while the sign-in screen shows, so the next session's
/// first page rises in its own colour rather than crossing over from the
/// last one's.
fn forget_glow(ctx: &Context) {
    ctx.data_mut(|data| data.remove::<Shown>(egui::Id::new(GLOW_ID)));
}

fn central(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let tint = page_tint(app);
    let page = app.page().clone();
    let overview = matches!(
        page,
        Page::Home | Page::Search | Page::Settings | Page::Queue
    );
    let strength = if overview { 0.45 } else { 0.85 };
    egui::CentralPanel::default()
        .frame(Frame::new().fill(palette.window))
        .show(ui, |ui| {
            let rect = ui.max_rect();
            // The wash belongs to the page's hero, so it scrolls away with
            // it. The page's offset is known once it has been laid out, so
            // the previous frame's serves.
            let scrolled_id = Id::new(("page-scrolled", page.encode()));
            let scrolled = ui
                .ctx()
                .data(|data| data.get_temp::<f32>(scrolled_id))
                .unwrap_or(0.0);
            // The glow rises in when a page or song brings a colour, and
            // crosses over to the next one, or to the palette's own wash,
            // rather than jumping.
            let rest = resting_glow(&palette);
            let (glow, moving) = glow(ui.ctx(), tint, strength, rest, app.motion.reduced());
            if moving {
                theme::animation_frame(ui.ctx());
            }
            if let Some((top, bottom)) = header_wash(&palette, glow) {
                widgets::paint_vertical_gradient(ui, header_rect(rect, scrolled), top, bottom);
            }
            ui.spacing_mut().item_spacing = vec2(8.0, 6.0);
            // egui fades a scrolled page's edge into the panel's plain
            // colour, which shows as a pale band over a cover's tint; the
            // page casts a shadow under the header instead.
            ui.spacing_mut().scroll.fade.strength = 0.0;
            topbar::show(app, ui);
            let scroll = crate::autoscroll::show(
                ui,
                egui::ScrollArea::vertical()
                    .id_salt(("page", page.encode()))
                    .auto_shrink([false, false]),
                egui::Vec2b::new(false, true),
                |ui| {
                    Frame::new()
                        .inner_margin(Margin {
                            left: widgets::PAGE_PADDING as i8,
                            right: widgets::PAGE_PADDING as i8,
                            top: 4,
                            bottom: 48,
                        })
                        .show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            match page {
                                Page::Home => home::show(app, ui),
                                Page::TopSongs => collection::top_songs(app, ui),
                                Page::Search => search::show(app, ui),
                                Page::LikedSongs => collection::liked(app, ui),
                                Page::Albums | Page::Artists | Page::Podcasts | Page::Episodes => {
                                    library::show(app, ui, page)
                                }
                                Page::Playlist(id) => collection::playlist(app, ui, &id),
                                Page::Album(id) => collection::album(app, ui, &id),
                                Page::Artist(id) => artist::show(app, ui, &id),
                                Page::Show(id) => show::show(app, ui, &id),
                                Page::Radio(seed) => radio::radio(app, ui, &seed),
                                Page::Queue => queue::page(app, ui),
                                Page::Settings => settings::show(app, ui),
                            }
                        });
                },
            );
            ui.ctx()
                .data_mut(|data| data.insert_temp(scrolled_id, scroll.state.offset.y));
            header_shadow(ui, scroll.inner_rect, scroll.state.offset.y, palette.dark);
        });
}

/// The gradient behind a page's header, top to bottom: the glow's colour
/// blended into the window at its strength, which is the art's, stronger
/// on a cover's own page than on an overview, or the palette's `tint`
/// (see [`resting_glow`]). `None` leaves the plain window.
fn header_wash(palette: &theme::Palette, glow: Glow) -> Option<(Color32, Color32)> {
    (glow.strength > 0.0).then(|| {
        (
            blend(palette.window, glow.color(), glow.strength),
            palette.window,
        )
    })
}

/// The glow a page without art settles on: the palette's `tint` as it
/// shows over the window, fading out down the header, or none when it is
/// transparent. `Color32` is premultiplied and egui blends in gamma space,
/// so this opaque colour paints exactly what the tint would, and the glow
/// can cross over between it and a cover's colour.
fn resting_glow(palette: &theme::Palette) -> Option<Glow> {
    let tint = palette.tint;
    if tint.a() == 0 {
        return None;
    }
    let behind = 1.0 - f32::from(tint.a()) / 255.0;
    let [r, g, b] = [0, 1, 2].map(|channel| {
        (f32::from(tint[channel]) + f32::from(palette.window[channel]) * behind)
            .round()
            .clamp(0.0, 255.0) as u8
    });
    Some(Glow::of(Color32::from_rgb(r, g, b), 1.0))
}

/// Where the header's wash is painted: the top of the panel, moved up by
/// how far the page has scrolled, so it leaves with the hero.
fn header_rect(panel: Rect, scrolled: f32) -> Rect {
    Rect::from_min_size(
        panel.min - vec2(0.0, scrolled.max(0.0)),
        vec2(panel.width(), 340.0),
    )
}

/// The shadow the header casts on a page scrolled under it, deepening over
/// the first few points of scrolling. Dark in both themes, lighter over a
/// light page.
fn header_shadow(ui: &egui::Ui, page: Rect, scrolled: f32, dark: bool) {
    let depth = (scrolled / 24.0).clamp(0.0, 1.0);
    if depth <= 0.0 {
        return;
    }
    let strength = if dark { 110.0 } else { 36.0 };
    let rect = Rect::from_min_size(page.min, vec2(page.width(), 14.0));
    widgets::paint_vertical_gradient(
        ui,
        rect,
        egui::Color32::from_black_alpha((strength * depth) as u8),
        egui::Color32::TRANSPARENT,
    );
}

/// Without a native title bar, the top of every panel drags the window, not
/// only the page's top bar.
const fn titlebar_spans_window(on_macos: bool, custom_titlebar: bool) -> bool {
    on_macos || custom_titlebar
}

/// The room the top bar's own controls need beside the caption buttons: the
/// 760-point minimum with the default 250-point sidebar leaves 510 points.
const TOPBAR_MIN_CONTENT: f32 = 510.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct WindowControlsReservation {
    pub topbar_width: f32,
    pub topbar_top: f32,
    pub queue_top: f32,
    pub lyrics_top: f32,
}

/// The caption buttons on the right belong to the outermost header: the
/// queue's or the lyrics' when either panel is open, otherwise the top bar,
/// beside its controls or above them when the page is too narrow.
fn controls_reservation(
    footprint: fastframe_titlebar::Footprint,
    queue: bool,
    lyrics: bool,
    topbar_width: f32,
) -> WindowControlsReservation {
    let mut space = WindowControlsReservation {
        topbar_width: 0.0,
        topbar_top: 0.0,
        queue_top: 0.0,
        lyrics_top: 0.0,
    };
    if footprint.right <= 0.0 {
        return space;
    }
    if queue {
        space.queue_top = footprint.height;
    } else if lyrics {
        space.lyrics_top = footprint.height;
    } else {
        let reserve = footprint.reserve(topbar_width, TOPBAR_MIN_CONTENT);
        space.topbar_width = reserve.inline_width;
        space.topbar_top = reserve.push_down;
    }
    space
}

/// Left-hand Linux controls sit above the sidebar's navigation rows.
pub(super) fn linux_left_controls_inset(ctx: &egui::Context) -> f32 {
    if !cfg!(target_os = "linux") {
        return 0.0;
    }
    let footprint = fastframe_titlebar::footprint(ctx, theme::CUSTOM_TOP_BAR_HEIGHT);
    if footprint.left > 0.0 {
        footprint.height
    } else {
        0.0
    }
}

pub(super) fn window_controls_reservation(
    ctx: &egui::Context,
    queue: bool,
    lyrics: bool,
    topbar_width: f32,
) -> WindowControlsReservation {
    controls_reservation(
        fastframe_titlebar::footprint(ctx, theme::CUSTOM_TOP_BAR_HEIGHT),
        queue,
        lyrics,
        topbar_width,
    )
}

/// Draws the caption buttons over the window's top corners in the palette's
/// colours: Windows' glyphs in the secondary text colour, GNOME's round
/// buttons in the text colour on the hover surface.
fn caption_buttons(ui: &mut egui::Ui, palette: &theme::Palette, locale: crate::i18n::Locale) {
    use crate::i18n::gettext;
    let style = fastframe_titlebar::Style {
        icon: if cfg!(target_os = "linux") {
            palette.text
        } else {
            palette.secondary
        },
        button: palette.surface_hover,
        header_height: theme::CUSTOM_TOP_BAR_HEIGHT,
    };
    // Literal calls, so xgettext extracts each label.
    let minimize = gettext(locale, "Minimize");
    let maximize = gettext(locale, "Maximize");
    let restore = gettext(locale, "Restore");
    let close = gettext(locale, "Close");
    let labels = fastframe_titlebar::Labels {
        minimize: &minimize,
        maximize: &maximize,
        restore: &restore,
        close: &close,
    };
    fastframe_titlebar::controls(ui, &style, &labels);
}

pub fn blend(base: Color32, tint: Color32, amount: f32) -> Color32 {
    let a = egui::Rgba::from(base);
    let b = egui::Rgba::from(tint);
    let mixed = a * (1.0 - amount) + b * amount;
    let mut color = Color32::from(mixed);
    color[3] = 255;
    color
}

/// How long a toast takes to fade in, and to fade out at the end of its
/// life.
const TOAST_FADE_IN: f32 = 0.15;
const TOAST_FADE_OUT: f32 = 0.4;

/// How much of a toast `age` seconds old shows: it fades in and, at the end
/// of its life, out. Less motion shows it whole for all of it.
fn toast_opacity(age: f32, still: bool) -> f32 {
    let life = crate::app::TOAST_LIFETIME.as_secs_f32();
    if still {
        1.0
    } else if age < TOAST_FADE_IN {
        age / TOAST_FADE_IN
    } else {
        ((life - age) / TOAST_FADE_OUT).clamp(0.0, 1.0)
    }
}

/// When the toasts of these `ages` next need a frame: at once while one
/// fades, otherwise when the next starts to fade out or, with less motion,
/// leaves. `None` with no toasts.
pub(crate) fn toast_wake(
    ages: impl Iterator<Item = f32>,
    still: bool,
) -> Option<std::time::Duration> {
    let life = crate::app::TOAST_LIFETIME.as_secs_f32();
    ages.map(|age| {
        let fades_out = life - TOAST_FADE_OUT;
        let wait = if still {
            life - age
        } else if age < TOAST_FADE_IN || age >= fades_out {
            0.0
        } else {
            fades_out - age
        };
        std::time::Duration::from_secs_f32(wait.max(0.0))
    })
    .min()
}

fn toasts(app: &mut App, ctx: &egui::Context, bottom_offset: f32) {
    if app.toasts.is_empty() {
        return;
    }
    let palette = app.palette;
    let still = app.motion.reduced();
    egui::Area::new(egui::Id::new("toasts"))
        .anchor(Align2::RIGHT_BOTTOM, vec2(-20.0, -bottom_offset))
        .order(egui::Order::Tooltip)
        .interactable(false)
        // Each toast fades itself in; the area's own fade would ask for
        // frames at the display's rate on top.
        .fade_in(false)
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            for toast in &app.toasts {
                let age = toast.created.elapsed().as_secs_f32();
                ui.set_opacity(toast_opacity(age, still));
                Frame::new()
                    .fill(palette.overlay)
                    .stroke(Stroke::new(1.0, palette.outline))
                    .corner_radius(CornerRadius::same(theme::RADIUS))
                    .inner_margin(Margin::symmetric(14, 10))
                    .shadow(egui::epaint::Shadow {
                        offset: [0, 4],
                        blur: 16,
                        spread: 0,
                        color: palette.shadow,
                    })
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let (icon, color) = match toast.kind {
                                ToastKind::Info => (Icon::CircleCheck, palette.accent),
                                ToastKind::Error => (Icon::CircleAlert, palette.danger),
                            };
                            theme::icon(ui, icon, 16.0, color);
                            // Laid out at its own width. The area
                            // remembers its size, so after a short toast a
                            // label left to wrap at the area's width broke
                            // long messages on every word.
                            let galley = ui.painter().layout(
                                toast.message.clone(),
                                theme::medium(13.5),
                                palette.text,
                                280.0,
                            );
                            ui.add(egui::Label::new(galley));
                        });
                    });
            }
        });
}

#[cfg(test)]
mod toast_tests {
    use super::*;
    use std::time::Duration;

    /// Toasts ask for frames only while one fades in or out, and wait for
    /// the next fade otherwise. With less motion they wait until one
    /// leaves, and show whole meanwhile.
    #[test]
    fn toasts_wake_the_window_only_to_fade() {
        let wake = |ages: &[f32], still| toast_wake(ages.iter().copied(), still);
        assert_eq!(wake(&[], false), None);
        assert_eq!(wake(&[0.05], false), Some(Duration::ZERO));
        let held = wake(&[1.0], false).unwrap().as_secs_f32();
        assert!((held - 1.8).abs() < 1e-3, "{held}");
        assert_eq!(wake(&[1.0, 2.9], false), Some(Duration::ZERO));
        let leaves = wake(&[1.0], true).unwrap().as_secs_f32();
        assert!((leaves - 2.2).abs() < 1e-3, "{leaves}");
        assert_eq!(toast_opacity(0.05, true), 1.0);
        assert!(toast_opacity(0.05, false) < 0.5);
        assert_eq!(toast_opacity(1.0, false), 1.0);
        assert!(toast_opacity(3.1, false) < 0.5);
    }
}

#[cfg(test)]
mod window_chrome_tests {
    use super::*;
    use fastframe_titlebar::Footprint;

    /// Windows' three buttons in its 48-point header.
    const BUTTONS: Footprint = Footprint {
        left: 0.0,
        right: 3.0 * 36.0 + 5.0,
        height: theme::CUSTOM_TOP_BAR_HEIGHT,
    };

    #[test]
    fn the_side_panels_drag_a_window_without_a_native_title_bar() {
        assert!(titlebar_spans_window(true, false));
        assert!(titlebar_spans_window(false, true));
        assert!(!titlebar_spans_window(false, false));
    }

    #[test]
    fn caption_space_belongs_to_the_outermost_header() {
        let values = |footprint, queue, lyrics| {
            let space = controls_reservation(footprint, queue, lyrics, f32::INFINITY);
            [
                space.topbar_width,
                space.topbar_top,
                space.queue_top,
                space.lyrics_top,
            ]
        };
        let [width, height] = [BUTTONS.right, BUTTONS.height];
        assert_eq!(values(BUTTONS, false, false), [width, 0.0, 0.0, 0.0]);
        assert_eq!(values(BUTTONS, true, false), [0.0, 0.0, height, 0.0]);
        assert_eq!(values(BUTTONS, false, true), [0.0, 0.0, 0.0, height]);
        assert_eq!(values(BUTTONS, true, true), [0.0, 0.0, height, 0.0]);
        // No buttons on the right: full screen, the native frame, macOS's
        // traffic lights, or a GNOME layout with them all on the left.
        let none = Footprint {
            left: 50.0,
            ..Footprint::default()
        };
        for (queue, lyrics) in [(false, false), (true, false), (false, true)] {
            assert_eq!(values(none, queue, lyrics), [0.0; 4]);
        }
    }

    #[test]
    fn minimum_windows_window_stacks_caption_space_above_the_topbar() {
        let available = 760.0 - 250.0;
        let space = controls_reservation(BUTTONS, false, false, available);
        assert_eq!(space.topbar_width, 0.0);
        assert_eq!(space.topbar_top, BUTTONS.height);

        let inline =
            controls_reservation(BUTTONS, false, false, TOPBAR_MIN_CONTENT + BUTTONS.right);
        assert_eq!(inline.topbar_width, BUTTONS.right);
        assert_eq!(inline.topbar_top, 0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A page fades from its cover's colour while art tints the interface,
    /// otherwise from the palette's own wash, and from nothing when the
    /// palette has none.
    #[test]
    fn the_header_fades_from_the_art_or_the_palettes_wash() {
        let mut palette = theme::Palette::dark();
        assert_eq!(resting_glow(&palette), None, "no wash of its own");
        assert_eq!(header_wash(&palette, Glow::DARK), None);

        let art = Color32::from_rgb(0x50, 0x38, 0xc8);
        let (page, bottom) = header_wash(&palette, Glow::of(art, 0.85)).unwrap();
        let (overview, _) = header_wash(&palette, Glow::of(art, 0.45)).unwrap();
        assert_eq!(page, blend(palette.window, art, 0.85));
        assert_eq!(overview, blend(palette.window, art, 0.45));
        assert_eq!(bottom, palette.window);

        // The palette's wash, painted as the colour the tint shows over the
        // window: black at alpha 24 over the window's own colour.
        palette.tint = Color32::from_black_alpha(24);
        let rest = resting_glow(&palette).unwrap();
        let (top, bottom) = header_wash(&palette, rest).unwrap();
        let shown = |channel: u8| (f32::from(channel) * (1.0 - 24.0 / 255.0)).round() as u8;
        let [r, g, b, _] = palette.window.to_array();
        assert_eq!(top, Color32::from_rgb(shown(r), shown(g), shown(b)));
        assert_eq!(bottom, palette.window);
        assert_eq!(
            header_wash(&palette, Glow::of(art, 0.85)).unwrap().0,
            page,
            "art wins over the wash"
        );
    }

    /// The wash sits at the top of an unscrolled page and leaves with the
    /// hero as the page scrolls, instead of staying behind the page.
    #[test]
    fn the_header_wash_scrolls_away_with_the_page() {
        let panel = Rect::from_min_size(egui::pos2(0.0, 80.0), vec2(800.0, 600.0));
        assert_eq!(header_rect(panel, 0.0).min, panel.min);
        assert_eq!(header_rect(panel, 0.0).width(), 800.0);
        assert_eq!(header_rect(panel, 120.0).min.y, panel.min.y - 120.0);
        assert_eq!(header_rect(panel, 120.0).max.y, panel.min.y + 220.0);
        assert!(
            header_rect(panel, 400.0).max.y < panel.min.y,
            "gone once scrolled past"
        );
        assert_eq!(
            header_rect(panel, -5.0).min,
            panel.min,
            "no overscroll bounce"
        );
    }

    /// The header casts a shadow only on a page scrolled under it, and it
    /// is black in both themes, never the page's own colour.
    #[test]
    fn the_header_shadow_appears_once_the_page_scrolls() {
        let ctx = egui::Context::default();
        let page = Rect::from_min_size(egui::pos2(0.0, 80.0), vec2(800.0, 600.0));
        let shadows = |scrolled: f32, dark: bool| {
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                header_shadow(ui, page, scrolled, dark);
            });
            output.textures_delta.clear();
            output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::Mesh(mesh) => Some(mesh.vertices.clone()),
                    _ => None,
                })
                .flatten()
                .collect::<Vec<_>>()
        };
        assert!(
            shadows(0.0, true).is_empty(),
            "nothing at the top of the page"
        );
        for dark in [true, false] {
            let vertices = shadows(40.0, dark);
            let top = vertices
                .iter()
                .find(|vertex| vertex.pos.y == page.top())
                .expect("a shadow along the page's top edge");
            assert!(top.color.a() > 0);
            assert_eq!((top.color.r(), top.color.g(), top.color.b()), (0, 0, 0));
        }
    }
}

#[cfg(test)]
mod glow_tests {
    use super::*;

    /// A pink glow at a page's full strength.
    fn pink() -> Glow {
        Glow::of(Color32::from_rgb(240, 80, 160), 0.85)
    }

    /// With a palette that has a wash of its own, a page without art
    /// settles on it, and the glow crosses over between it and a cover's
    /// colour instead of jumping.
    #[test]
    fn the_glow_rests_on_the_palettes_wash() {
        let mut palette = theme::Palette::dark();
        palette.tint = Color32::from_rgba_unmultiplied(0x30, 0x60, 0x90, 40);
        let rest = resting_glow(&palette);
        let ctx = egui::Context::default();
        let frame = |time: f64, tint: PageTint| {
            let mut result = (Glow::DARK, false);
            let mut output = ctx.run_ui(
                egui::RawInput {
                    time: Some(time),
                    ..Default::default()
                },
                |ui| result = glow(ui.ctx(), tint, 0.85, rest, false),
            );
            output.textures_delta.clear();
            result
        };
        let settle = |time: &mut f64, tint: PageTint| {
            for _ in 0..240 {
                let (glow, moving) = frame(*time, tint);
                *time += 1.0 / 60.0;
                if !moving {
                    return glow;
                }
            }
            panic!("the glow never settled");
        };
        let mut time = 0.0;
        assert_eq!(settle(&mut time, PageTint::None), rest.unwrap());
        let art = PageTint::Color(pink().color());
        let (between, moving) = frame(time, art);
        assert!(moving, "crosses over to the cover's colour");
        assert!(between != rest.unwrap() && between != pink());
        time += 1.0 / 60.0;
        assert_eq!(settle(&mut time, art), pink());
        let (leaving, _) = frame(time, PageTint::None);
        assert!(leaving.strength > 0.85, "heads back for the wash, not out");
    }

    /// The palette's wash shows from the first frame, and a new palette's
    /// wash, dark like the last, lands at once under the theme's reveal.
    #[test]
    fn a_palettes_wash_is_there_at_once_and_a_new_one_lands() {
        let wash = |tint: Color32| {
            let mut palette = theme::Palette::dark();
            palette.tint = tint;
            resting_glow(&palette)
        };
        let first = wash(Color32::from_rgba_unmultiplied(0x30, 0x60, 0x90, 40));
        let second = wash(Color32::from_rgba_unmultiplied(0x90, 0x30, 0x60, 60));
        let ctx = egui::Context::default();
        let frame = |time: f64, rest: Option<Glow>| {
            let mut result = (Glow::DARK, false);
            let mut output = ctx.run_ui(
                egui::RawInput {
                    time: Some(time),
                    ..Default::default()
                },
                |ui| result = glow(ui.ctx(), PageTint::None, 0.85, rest, false),
            );
            output.textures_delta.clear();
            result
        };
        assert_eq!(frame(0.0, first), (first.unwrap(), false), "no rise in");
        assert_eq!(frame(1.0 / 60.0, second), (second.unwrap(), false));
    }

    /// A glow from nothing takes its colour at once, rises in it and settles
    /// within a second.
    #[test]
    fn the_glow_rises_in_its_own_colour() {
        let mut glow = Glow::DARK;
        glow = glow.toward(pink(), 1.0 / 60.0, false);
        assert_eq!(glow.rgb, pink().rgb, "no fade through black");
        assert!(
            glow.strength > 0.0 && glow.strength < 0.2,
            "{}",
            glow.strength
        );
        let mut frames = 1;
        while glow != pink() && frames < 120 {
            glow = glow.toward(pink(), 1.0 / 60.0, false);
            frames += 1;
        }
        assert!(
            frames <= 50,
            "settled in {frames} frames, not easing on unseen"
        );
    }

    /// A glow that shows moves towards a new colour in steps; less motion
    /// lands on it.
    #[test]
    fn a_new_colour_crosses_over_rather_than_jumping() {
        let blue = Glow::of(Color32::from_rgb(40, 90, 230), 0.85);
        let glow = pink().toward(blue, 1.0 / 60.0, false);
        assert!(glow.rgb[0] < pink().rgb[0] && glow.rgb[0] > blue.rgb[0]);
        assert_eq!(pink().toward(blue, 1.0 / 60.0, true), blue, "less motion");
    }

    /// While a page's colour is being read the glow holds what it shows,
    /// and a page without a colour lets it fade.
    #[test]
    fn a_pending_colour_holds_and_none_fades() {
        let ctx = egui::Context::default();
        let frame = |time: f64, tint: PageTint| {
            let mut result = (Glow::DARK, false);
            let mut output = ctx.run_ui(
                egui::RawInput {
                    time: Some(time),
                    ..Default::default()
                },
                |ui| result = glow(ui.ctx(), tint, 0.85, None, false),
            );
            output.textures_delta.clear();
            result
        };
        let mut time = 0.0;
        let pink_tint = PageTint::Color(pink().color());
        for _ in 0..120 {
            if !frame(time, pink_tint).1 {
                break;
            }
            time += 1.0 / 60.0;
        }
        let (held, moving) = frame(time + 0.5, PageTint::Pending);
        assert_eq!(held, pink());
        assert!(!moving);
        let (fading, moving) = frame(time + 0.52, PageTint::None);
        assert!(fading.strength < 0.85 && moving);
        assert_eq!(fading.rgb, pink().rgb);
    }

    /// A page that starts reading its colour halfway through a crossing
    /// keeps heading where the glow was going, rather than stopping on the
    /// colour between.
    #[test]
    fn a_pending_colour_keeps_heading_for_the_last_one() {
        let ctx = egui::Context::default();
        let frame = |time: f64, tint: PageTint| {
            let mut result = (Glow::DARK, false);
            let mut output = ctx.run_ui(
                egui::RawInput {
                    time: Some(time),
                    ..Default::default()
                },
                |ui| result = glow(ui.ctx(), tint, 0.85, None, false),
            );
            output.textures_delta.clear();
            result
        };
        let blue = Glow::of(Color32::from_rgb(40, 90, 230), 0.85);
        let mut time = 0.0;
        for _ in 0..120 {
            if !frame(time, PageTint::Color(pink().color())).1 {
                break;
            }
            time += 1.0 / 60.0;
        }
        time += 1.0 / 60.0;
        let (between, moving) = frame(time, PageTint::Color(blue.color()));
        assert!(moving && between != blue);
        let mut glow = between;
        for _ in 0..120 {
            time += 1.0 / 60.0;
            let (now, moving) = frame(time, PageTint::Pending);
            glow = now;
            if !moving {
                break;
            }
        }
        assert_eq!(glow, blue, "it lands where it was going");
    }

    /// A change of theme lands the glow on the new theme's tint at once,
    /// under the theme's own reveal, and the sign-in screen forgets it.
    #[test]
    fn a_new_theme_lands_the_glow_and_signing_out_forgets_it() {
        let ctx = egui::Context::default();
        let frame = |time: f64, dark: bool, tint: PageTint| {
            ctx.set_theme(if dark {
                egui::Theme::Dark
            } else {
                egui::Theme::Light
            });
            let mut result = (Glow::DARK, false);
            let mut output = ctx.run_ui(
                egui::RawInput {
                    time: Some(time),
                    ..Default::default()
                },
                |ui| result = glow(ui.ctx(), tint, 0.85, None, false),
            );
            output.textures_delta.clear();
            result
        };
        let dark_tint = Color32::from_rgb(120, 40, 90);
        let light_tint = Color32::from_rgb(240, 160, 200);
        let mut time = 0.0;
        for _ in 0..120 {
            if !frame(time, true, PageTint::Color(dark_tint)).1 {
                break;
            }
            time += 1.0 / 60.0;
        }
        let (landed, moving) = frame(time + 1.0 / 60.0, false, PageTint::Color(light_tint));
        assert_eq!(landed, Glow::of(light_tint, 0.85));
        assert!(!moving);
        forget_glow(&ctx);
        let (first, _) = frame(time + 2.0 / 60.0, false, PageTint::Color(dark_tint));
        assert_eq!(first.rgb, Glow::of(dark_tint, 0.85).rgb, "no crossing");
        assert!(first.strength < 0.2, "it rises in from nothing");
    }
}
