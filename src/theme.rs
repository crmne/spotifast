//! Shared palette, typography, icons, and base widgets.
//!
//! Inter provides real font weights, and Lucide provides a consistent icon set.
//! All colors use [`Palette`] so light, dark, and album-art-tinted themes stay
//! consistent.

#[cfg(target_os = "linux")]
mod omarchy;

use crate::i18n::{Locale, gettext};
use egui::{Color32, CornerRadius, Response, Sense, Stroke, Vec2};
use std::borrow::Cow;

/// A palette file from the themes directory.
pub type CustomTheme = fastframe_theme::CustomTheme<Palette>;
/// The palette files, and the Omarchy palette where the desktop has one.
pub type Catalog = fastframe_theme::Catalog<Palette>;

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Palette {
    pub dark: bool,
    pub window: Color32,
    pub panel: Color32,
    pub surface: Color32,
    pub surface_hover: Color32,
    pub surface_active: Color32,
    pub outline: Color32,
    pub text: Color32,
    pub secondary: Color32,
    pub dim: Color32,
    pub accent: Color32,
    pub accent_hover: Color32,
    pub on_accent: Color32,
    pub danger: Color32,
    pub warning: Color32,
    pub overlay: Color32,
    pub shadow: Color32,
}

impl Palette {
    pub fn dark() -> Self {
        Self {
            dark: true,
            window: Color32::from_rgb(0x0f, 0x11, 0x14),
            panel: Color32::from_rgb(0x15, 0x18, 0x1c),
            surface: Color32::from_rgb(0x1d, 0x21, 0x27),
            surface_hover: Color32::from_rgb(0x26, 0x2b, 0x33),
            surface_active: Color32::from_rgb(0x2f, 0x35, 0x3f),
            outline: Color32::from_rgb(0x2a, 0x30, 0x38),
            text: Color32::from_rgb(0xf2, 0xf4, 0xf6),
            secondary: Color32::from_rgb(0xa9, 0xb1, 0xbc),
            dim: Color32::from_rgb(0x6e, 0x77, 0x84),
            accent: Color32::from_rgb(0x1e, 0xd7, 0x60),
            accent_hover: Color32::from_rgb(0x3c, 0xe8, 0x7a),
            on_accent: Color32::from_rgb(0x0a, 0x14, 0x0e),
            danger: Color32::from_rgb(0xf5, 0x71, 0x7f),
            warning: Color32::from_rgb(0xf2, 0xb8, 0x5c),
            overlay: Color32::from_rgb(0x22, 0x27, 0x2e),
            shadow: Color32::from_black_alpha(140),
        }
    }

    pub fn light() -> Self {
        Self {
            dark: false,
            window: Color32::from_rgb(0xf8, 0xf9, 0xfb),
            panel: Color32::from_rgb(0xff, 0xff, 0xff),
            surface: Color32::from_rgb(0xee, 0xf0, 0xf3),
            surface_hover: Color32::from_rgb(0xe3, 0xe6, 0xeb),
            surface_active: Color32::from_rgb(0xd7, 0xdb, 0xe1),
            outline: Color32::from_rgb(0xdd, 0xe1, 0xe6),
            text: Color32::from_rgb(0x14, 0x17, 0x1a),
            secondary: Color32::from_rgb(0x53, 0x5b, 0x66),
            dim: Color32::from_rgb(0x8b, 0x93, 0x9e),
            accent: Color32::from_rgb(0x15, 0xa6, 0x4a),
            accent_hover: Color32::from_rgb(0x12, 0x8f, 0x40),
            on_accent: Color32::WHITE,
            danger: Color32::from_rgb(0xd6, 0x3b, 0x4c),
            warning: Color32::from_rgb(0xb8, 0x7a, 0x14),
            overlay: Color32::from_rgb(0xff, 0xff, 0xff),
            shadow: Color32::from_black_alpha(50),
        }
    }

    /// A colour derived from album art, softened so it can sit behind text.
    pub fn tint_from_art(&self, rgb: [u8; 3]) -> Color32 {
        let [r, g, b] = rgb.map(|c| c as f32 / 255.0);
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let lightness = (max + min) / 2.0;
        let target = if self.dark { 0.30 } else { 0.72 };
        let (r, g, b) = if lightness < 0.01 {
            (target, target, target)
        } else {
            let scale = target / lightness;
            (
                (r * scale).min(1.0),
                (g * scale).min(1.0),
                (b * scale).min(1.0),
            )
        };
        Color32::from_rgb((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8)
    }
}

impl fastframe_theme::Palette for Palette {
    fn base(base: fastframe_theme::Base) -> Self {
        match base {
            fastframe_theme::Base::Dark => Self::dark(),
            fastframe_theme::Base::Light => Self::light(),
        }
    }

    fn set(&mut self, name: &str, color: Color32) -> bool {
        match name {
            "window" => self.window = color,
            "panel" => self.panel = color,
            "surface" => self.surface = color,
            "surface_hover" => self.surface_hover = color,
            "surface_active" => self.surface_active = color,
            "outline" => self.outline = color,
            "text" => self.text = color,
            "secondary" => self.secondary = color,
            "dim" => self.dim = color,
            "accent" => self.accent = color,
            "accent_hover" => self.accent_hover = color,
            "on_accent" => self.on_accent = color,
            "danger" => self.danger = color,
            "warning" => self.warning = color,
            "overlay" => self.overlay = color,
            "shadow" => self.shadow = color,
            _ => return false,
        }
        true
    }
}

/// Whether a launch may follow the desktop's themes with this themes
/// folder. An updater trial of the legacy Fastpotify profile does not, so the
/// old hook and profile stay together until the update is accepted.
fn desktop_themes_allowed(themes: &std::path::Path) -> bool {
    themes != crate::paths::AppDirs::legacy().config.join("themes")
}

/// Adds the desktop's palettes to a normal launch: the eight shared palettes,
/// installed into the themes folder as files on the first launch, and
/// Omarchy's on Linux, with the packaged template and hook installed for the
/// user.
pub fn enable_desktop_themes(catalog: &mut Catalog, themes: &std::path::Path) {
    if !desktop_themes_allowed(themes) {
        return;
    }
    #[cfg(target_os = "linux")]
    omarchy::upgrade_legacy_hook();
    catalog.enable_desktop_themes(fastframe_theme::DesktopThemes {
        slug: "spotifast",
        omarchy_template: include_str!("../contrib/omarchy/spotifast.json.tpl"),
        // The template has not changed since it first shipped.
        omarchy_previous_templates: &[],
        presets: true,
    });
}

/// The status line under the Theme setting, empty when all is well.
pub fn catalog_detail(
    catalog: &Catalog,
    locale: Locale,
    selected: Option<&str>,
) -> Cow<'static, str> {
    use fastframe_theme::{Problem, Status};
    let Some(status) = catalog.status(selected) else {
        return Cow::Borrowed("");
    };
    match status {
        Status::Loading => gettext(locale, "Loading local themes…"),
        Status::SelectedUnavailable => gettext(
            locale,
            "The selected theme is unavailable. Keeping the last usable appearance. See the log for details.",
        ),
        Status::Problem(Problem::Unreadable) => gettext(
            locale,
            "The themes folder could not be read. See the log for details.",
        ),
        Status::Problem(Problem::TooManyEntries) => gettext(
            locale,
            "The themes folder has more than 512 entries. Keep fewer files there to list the custom palettes.",
        ),
        Status::Problem(Problem::TooManyThemes) => gettext(
            locale,
            "Only 128 custom palettes can be listed. Keep fewer JSON files in the themes folder to see the rest.",
        ),
        Status::Problem(Problem::OmarchyUnreadable) => gettext(
            locale,
            "The Omarchy palette could not be loaded. Keeping the last usable appearance. See the log for details.",
        ),
        Status::Problem(_) => gettext(
            locale,
            "Custom themes could not be loaded. Run spotifast reload-themes to try again.",
        ),
    }
}

pub const RADIUS: u8 = 8;
pub const RADIUS_SMALL: u8 = 4;
pub const ROW_HEIGHT: f32 = 56.0;
pub const COMPACT_ROW_HEIGHT: f32 = 48.0;
/// The compact track list: one line, no cover.
pub const THIN_ROW_HEIGHT: f32 = 36.0;
pub const PLAYER_BAR_HEIGHT: f32 = 88.0;
/// The narrowest either right-hand panel goes. The queue and the lyrics
/// take the same edge and swap places there, so a width that suits one
/// has to suit the other, or the window would jump on the swap.
pub const SIDE_PANEL_MIN_WIDTH: f32 = 280.0;
pub const TOP_BAR_HEIGHT: f32 = 56.0;

/// macOS hides the titlebar and draws the window content all the way to the
/// top edge, so whatever sits at the top of the window has to leave room for
/// the traffic lights. Zero everywhere else, and in fullscreen, where the
/// buttons are gone.
pub fn titlebar_inset(ctx: &egui::Context) -> f32 {
    if cfg!(target_os = "macos") && !ctx.input(|input| input.viewport().fullscreen.unwrap_or(false))
    {
        28.0
    } else {
        0.0
    }
}

pub fn regular(size: f32) -> egui::FontId {
    fastframe_fonts::Weight::Regular.font_id(size)
}

pub fn medium(size: f32) -> egui::FontId {
    fastframe_fonts::Weight::Medium.font_id(size)
}

pub fn semibold(size: f32) -> egui::FontId {
    fastframe_fonts::Weight::SemiBold.font_id(size)
}

pub fn bold(size: f32) -> egui::FontId {
    fastframe_fonts::Weight::Bold.font_id(size)
}

/// How the desktop renders text, read once per process.
///
/// Tests use the platform's default instead of asking the desktop, so they
/// neither wait on D-Bus nor depend on the machine's settings.
pub fn text_rendering() -> fastframe_text::TextRendering {
    static RENDERING: std::sync::OnceLock<fastframe_text::TextRendering> =
        std::sync::OnceLock::new();
    *RENDERING.get_or_init(|| {
        if cfg!(test) {
            fastframe_text::TextRendering::platform_default()
        } else {
            fastframe_text::detect()
        }
    })
}

/// Install fonts, icons, and the base style once.
pub fn install(ctx: &egui::Context) {
    install_fonts(ctx);
    egui_extras::install_image_loaders(ctx);
    fastframe_icons::install::<Icon>(ctx);
}

/// Applies the palette to egui's own widgets so dialogs, menus, and text
/// fields agree with the custom views.
pub fn apply(ctx: &egui::Context, palette: &Palette) {
    let mut style = (*ctx.global_style()).clone();
    apply_to_style(&mut style, palette);
    ctx.set_global_style(style);
}

/// Applies a palette to this view and children without changing global style.
pub fn apply_local(ui: &mut egui::Ui, palette: &Palette) {
    apply_to_style(ui.style_mut(), palette);
}

fn apply_to_style(style: &mut egui::Style, palette: &Palette) {
    let visuals = &mut style.visuals;
    *visuals = if palette.dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    visuals.dark_mode = palette.dark;
    // Glyph coverage, hinting and sub-pixel positions as the desktop draws
    // them: linear coverage in both themes on Linux, where egui's dark curve
    // (2c - c²) made text heavier than GTK's.
    text_rendering().apply_to_visuals(visuals);
    visuals.panel_fill = palette.panel;
    visuals.window_fill = palette.overlay;
    visuals.extreme_bg_color = palette.surface;
    visuals.faint_bg_color = palette.surface;
    visuals.code_bg_color = palette.surface;
    visuals.override_text_color = Some(palette.text);
    visuals.weak_text_color = Some(palette.secondary);
    visuals.hyperlink_color = palette.text;
    visuals.selection.bg_fill = palette.accent.gamma_multiply(0.35);
    visuals.selection.stroke = Stroke::new(1.0, palette.accent);
    visuals.window_stroke = Stroke::new(1.0, palette.outline);
    visuals.window_corner_radius = CornerRadius::same(RADIUS + 2);
    visuals.menu_corner_radius = CornerRadius::same(RADIUS);
    visuals.window_shadow = egui::epaint::Shadow {
        offset: [0, 6],
        blur: 24,
        spread: 0,
        color: palette.shadow,
    };
    visuals.popup_shadow = egui::epaint::Shadow {
        offset: [0, 4],
        blur: 16,
        spread: 0,
        color: palette.shadow,
    };
    let corner = CornerRadius::same(RADIUS_SMALL + 2);
    for widget in [
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        widget.corner_radius = corner;
        widget.bg_stroke = Stroke::NONE;
        widget.fg_stroke = Stroke::new(1.0, palette.text);
        widget.expansion = 0.0;
    }
    visuals.widgets.noninteractive.corner_radius = corner;
    visuals.widgets.noninteractive.bg_fill = palette.panel;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, palette.outline);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, palette.text);
    visuals.widgets.inactive.bg_fill = palette.surface;
    visuals.widgets.inactive.weak_bg_fill = palette.surface;
    visuals.widgets.hovered.bg_fill = palette.surface_hover;
    visuals.widgets.hovered.weak_bg_fill = palette.surface_hover;
    visuals.widgets.active.bg_fill = palette.surface_active;
    visuals.widgets.active.weak_bg_fill = palette.surface_active;
    visuals.widgets.open.bg_fill = palette.surface_hover;
    visuals.widgets.open.weak_bg_fill = palette.surface_hover;
    visuals.text_cursor.stroke = Stroke::new(2.0, palette.accent);
    visuals.striped = false;
    visuals.slider_trailing_fill = true;
    visuals.handle_shape = egui::style::HandleShape::Circle;

    use egui::FontFamily::{Monospace, Proportional};
    use egui::{FontId, TextStyle};
    style.text_styles = [
        (TextStyle::Small, FontId::new(11.5, Proportional)),
        (TextStyle::Body, FontId::new(14.0, Proportional)),
        (TextStyle::Button, FontId::new(14.0, Proportional)),
        (TextStyle::Heading, FontId::new(22.0, Proportional)),
        (TextStyle::Monospace, FontId::new(13.0, Monospace)),
    ]
    .into();
    style.spacing.item_spacing = Vec2::new(8.0, 6.0);
    style.spacing.button_padding = Vec2::new(12.0, 6.0);
    style.spacing.interact_size = Vec2::new(40.0, 28.0);
    style.spacing.menu_margin = egui::Margin::same(6);
    style.spacing.window_margin = egui::Margin::same(16);
    style.spacing.scroll = egui::style::ScrollStyle {
        bar_width: 8.0,
        floating_width: 6.0,
        floating_allocated_width: 0.0,
        handle_min_length: 28.0,
        bar_inner_margin: 3.0,
        bar_outer_margin: 2.0,
        dormant_background_opacity: 0.0,
        dormant_handle_opacity: 0.0,
        active_background_opacity: 0.0,
        active_handle_opacity: 0.55,
        interact_handle_opacity: 0.85,
        foreground_color: true,
        ..egui::style::ScrollStyle::floating()
    };
    style.interaction.selectable_labels = false;
    style.interaction.tooltip_delay = 0.4;
    style.animation_time = 0.12;
    style.url_in_tooltip = false;
}

/// Inter at its four weights with the monochrome emoji face right behind it
/// (so every emoji wears the same style, ahead of egui's own pair), then the
/// installed faces for the scripts Inter lacks, drawn the way the desktop
/// renders text.
fn install_fonts(ctx: &egui::Context) {
    let emoji = egui::FontData::from_static(include_bytes!("../assets/fonts/NotoEmoji.ttf"));
    let mut fonts = fastframe_fonts::FontSetup::default()
        .companion("noto_emoji", std::sync::Arc::new(emoji))
        .definitions();
    text_rendering().apply_to(&mut fonts);
    ctx.set_fonts(fonts);
}

fastframe_icons::icons! {
    /// Every icon the interface draws. The shared Lucide icons come from
    /// fastframe-icons; the rest are Spotifast's own files.
    pub enum Icon {
        prefix: "spotifast-icon-",
        directory: "../assets/icons/",
        ArrowLeft => lucide "arrow-left",
        ArrowRight => "arrow-right",
        AudioLines => "audio-lines",
        BadgeCheck => "badge-check",
        Bookmark => "bookmark",
        BookmarkFilled => "bookmark-filled",
        Car => "car",
        Cast => "cast",
        Check => lucide "check",
        ChevronDown => lucide "chevron-down",
        ChevronLeft => lucide "chevron-left",
        ChevronRight => lucide "chevron-right",
        ChevronUp => lucide "chevron-up",
        CircleAlert => lucide "circle-alert",
        CircleCheck => lucide "circle-check",
        CirclePlay => "circle-play",
        CirclePlus => "circle-plus",
        CircleX => lucide "circle-x",
        Clock => lucide "clock",
        Compass => "compass",
        Copy => lucide "copy",
        Disc => "disc-3",
        Ellipsis => lucide "ellipsis",
        Expand => "expand",
        ExternalLink => lucide "external-link",
        Gamepad => "gamepad-2",
        Globe => "globe",
        GripVertical => "grip-vertical",
        Headphones => "headphones",
        Heart => "heart",
        HeartFilled => "heart-filled",
        House => "house",
        Info => lucide "info",
        Laptop => "laptop",
        Library => "library",
        LayoutGrid => "layout-grid",
        LayoutList => "layout-list",
        ListEnd => "list-end",
        ListMusic => "list-music",
        ListPlus => "list-plus",
        ListVideo => "list-video",
        Loader => "loader-circle",
        Lock => lucide "lock",
        LogOut => lucide "log-out",
        Mic => lucide "mic",
        Minus => lucide "minus",
        Monitor => lucide "monitor",
        Moon => lucide "moon",
        Music => "music",
        Pause => lucide "pause",
        PauseFilled => "pause-filled",
        PanelLeft => lucide "panel-left",
        Pin => lucide "pin",
        PinOff => lucide "pin-off",
        Pencil => lucide "pencil",
        Play => lucide "play",
        PlayFilled => "play-filled",
        Plus => lucide "plus",
        Radio => "radio",
        Refresh => lucide "refresh-cw",
        Repeat => "repeat",
        Repeat1 => "repeat-1",
        Search => lucide "search",
        Settings => lucide "settings",
        Shrink => "shrink",
        Shuffle => "shuffle",
        SkipBack => "skip-back",
        SkipBackFilled => "skip-back-filled",
        SkipForward => "skip-forward",
        SkipForwardFilled => "skip-forward-filled",
        Smartphone => lucide "smartphone",
        Sparkles => "sparkles",
        Speaker => "speaker",
        Square => "square",
        SquarePen => lucide "square-pen",
        Sun => lucide "sun",
        Tablet => "tablet",
        Trash => lucide "trash-2",
        TrendingUp => "trending-up",
        Tv => "tv",
        User => lucide "user",
        Users => lucide "users",
        Volume => "volume",
        Volume1 => "volume-1",
        Volume2 => lucide "volume-2",
        VolumeX => lucide "volume-x",
        Watch => "watch",
        X => lucide "x",
        Zap => "zap",
    }
}

/// A static icon.
pub fn icon(ui: &mut egui::Ui, icon: Icon, size: f32, color: Color32) -> Response {
    ui.add(icon.image(color, size))
}

/// Paints an icon centred in `rect` without allocating space.
pub fn paint_icon(ui: &egui::Ui, icon: Icon, rect: egui::Rect, size: f32, color: Color32) {
    let icon_rect = egui::Rect::from_center_size(
        rect.center() + play_glyph_offset(icon, size),
        Vec2::splat(size),
    );
    icon.image(color, size).paint_at(ui, icon_rect);
}

/// Make keyboard focus visible without changing the control's layout.
pub fn focus_ring(ui: &egui::Ui, response: &Response) {
    if response.has_focus() {
        ui.painter().rect_stroke(
            response.rect.expand(2.0),
            4.0,
            ui.visuals().selection.stroke,
            egui::StrokeKind::Outside,
        );
    }
    if response.gained_focus() {
        response.scroll_to_me(None);
    }
}

/// A frameless icon control whose colour lifts on hover.
pub fn icon_button(
    ui: &mut egui::Ui,
    icon: Icon,
    size: f32,
    color: Color32,
    hover: Color32,
    tooltip: &str,
) -> Response {
    let edge = size + 12.0;
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(edge), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), tooltip)
    });
    if ui.is_rect_visible(rect) {
        let tint = if response.hovered() || response.has_focus() {
            hover
        } else {
            color
        };
        let scale = if response.is_pointer_button_down_on() {
            0.92
        } else {
            1.0
        };
        paint_icon(ui, icon, rect, size * scale, tint);
    }
    focus_ring(ui, &response);
    if tooltip.is_empty() {
        response
    } else {
        response.on_hover_text(tooltip)
    }
}

/// Horizontal offset that optically centers play triangles.
///
/// Lucide includes a 1/24-width shift; a measured 3% shift centers the icon at
/// Spotifast's sizes. Use this everywhere instead of per-call adjustments.
pub fn play_glyph_offset(icon: Icon, icon_size: f32) -> Vec2 {
    if matches!(icon, Icon::PlayFilled | Icon::Play) {
        Vec2::new(icon_size * (0.03 - 1.0 / 24.0), 0.0)
    } else {
        Vec2::ZERO
    }
}

/// The app's mark, the accent disc with the play triangle, drawn the same
/// wherever it appears.
pub fn logo(ui: &egui::Ui, center: egui::Pos2, diameter: f32, disc: Color32, glyph: Color32) {
    ui.painter().circle_filled(center, diameter / 2.0, disc);
    let icon_size = diameter * 0.45;
    let icon_rect = egui::Rect::from_center_size(
        center + play_glyph_offset(Icon::PlayFilled, icon_size),
        Vec2::splat(icon_size),
    );
    Icon::PlayFilled
        .image(glyph, icon_size)
        .paint_at(ui, icon_rect);
}

pub fn circle_button(
    ui: &mut egui::Ui,
    icon: Icon,
    diameter: f32,
    fill: Color32,
    fill_hover: Color32,
    icon_color: Color32,
    tooltip: &str,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(diameter), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), tooltip)
    });
    if ui.is_rect_visible(rect) {
        let hovered = response.hovered();
        let grow = if hovered { 1.05 } else { 1.0 };
        let radius = diameter / 2.0 * grow;
        let fill = if hovered { fill_hover } else { fill };
        ui.painter().circle_filled(rect.center(), radius, fill);
        let icon_size = diameter * 0.46;
        let offset = play_glyph_offset(icon, icon_size);
        let icon_rect =
            egui::Rect::from_center_size(rect.center() + offset, Vec2::splat(icon_size));
        icon.image(icon_color, icon_size).paint_at(ui, icon_rect);
    }
    focus_ring(ui, &response);
    if tooltip.is_empty() {
        response
    } else {
        response.on_hover_text(tooltip)
    }
}

/// A disc the size of a [`circle_button`] whose icon is replaced by a
/// spinner: the pressed play button itself shows that Spotify is reacting.
pub fn circle_spinner(
    ui: &mut egui::Ui,
    diameter: f32,
    fill: Color32,
    spin: Color32,
    tooltip: &str,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(diameter), Sense::hover());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::ProgressIndicator,
            ui.is_enabled(),
            tooltip,
        )
    });
    if ui.is_rect_visible(rect) {
        ui.painter()
            .circle_filled(rect.center(), diameter / 2.0, fill);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(
            egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
        ));
        spinner(&mut child, diameter * 0.55, spin);
    }
    if tooltip.is_empty() {
        response
    } else {
        response.on_hover_text(tooltip)
    }
}

/// A pill-shaped text button: filled for the primary action, outlined otherwise.
pub fn pill_button(ui: &mut egui::Ui, palette: &Palette, label: &str, primary: bool) -> Response {
    let font = semibold(13.0);
    let color = if primary {
        palette.on_accent
    } else {
        palette.text
    };
    let galley = ui.painter().layout_no_wrap(label.to_string(), font, color);
    let padding = Vec2::new(18.0, 8.0);
    let size = galley.size() + padding * 2.0;
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
    });
    if ui.is_rect_visible(rect) {
        let hovered = response.hovered();
        let radius = rect.height() / 2.0;
        if primary {
            let fill = if hovered {
                palette.accent_hover
            } else {
                palette.accent
            };
            ui.painter().rect_filled(rect, radius, fill);
        } else {
            let stroke_color = if hovered { palette.text } else { palette.dim };
            ui.painter().rect_stroke(
                rect,
                radius,
                Stroke::new(1.0, stroke_color),
                egui::StrokeKind::Inside,
            );
        }
        let pos = rect.center() - galley.size() / 2.0;
        ui.painter().galley(pos, galley, color);
    }
    focus_ring(ui, &response);
    response
}

/// A muted button with an icon and label, for row and header actions.
pub fn soft_button(
    ui: &mut egui::Ui,
    palette: &Palette,
    icon: Option<Icon>,
    label: &str,
    active: bool,
) -> Response {
    soft_button_inner(ui, palette, icon, label, active, false).0
}

/// A [`soft_button`] whose icon turns into a cross on hover, allowing the
/// entry to be dismissed. The returned flag reports clicks on that cross.
pub fn soft_button_dismiss(
    ui: &mut egui::Ui,
    palette: &Palette,
    icon: Icon,
    label: &str,
) -> (Response, bool) {
    soft_button_inner(ui, palette, Some(icon), label, false, true)
}

/// The width `soft_button` gives a button without an icon, for laying out
/// a row of them before drawing it.
pub fn soft_button_width(ui: &egui::Ui, label: &str) -> f32 {
    let galley = crate::bidi::layout_line(ui.painter(), label, medium(13.0), Color32::WHITE);
    galley.size().x + 24.0
}

fn soft_button_inner(
    ui: &mut egui::Ui,
    palette: &Palette,
    icon: Option<Icon>,
    label: &str,
    active: bool,
    dismissible: bool,
) -> (Response, bool) {
    let font = medium(13.0);
    let color = if active { palette.window } else { palette.text };
    let galley = crate::bidi::layout_line(ui.painter(), label, font, color);
    let icon_size = 15.0;
    let icon_width = if icon.is_some() { icon_size + 6.0 } else { 0.0 };
    let padding = Vec2::new(12.0, 7.0);
    let size = Vec2::new(galley.size().x + icon_width, galley.size().y) + padding * 2.0;
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
    });
    let icon_rect = egui::Rect::from_center_size(
        egui::pos2(rect.left() + padding.x + icon_size / 2.0, rect.center().y),
        Vec2::splat(icon_size),
    );
    // Claimed after the button, so the cross sits on top and keeps its own click.
    let dismiss = (dismissible && icon.is_some()).then(|| {
        let dismiss = ui.interact(
            icon_rect.expand(3.0),
            response.id.with("dismiss"),
            Sense::click(),
        );
        dismiss.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Button,
                ui.is_enabled(),
                format!("Remove {label}"),
            )
        });
        dismiss
    });
    let dismissed = dismiss.as_ref().is_some_and(Response::clicked);
    let over_dismiss = dismiss
        .as_ref()
        .is_some_and(|dismiss| dismiss.hovered() || dismiss.has_focus());
    if ui.is_rect_visible(rect) {
        let hovered = response.hovered() || over_dismiss;
        let fill = if active {
            palette.text
        } else if hovered {
            palette.surface_hover
        } else {
            palette.surface
        };
        ui.painter().rect_filled(rect, rect.height() / 2.0, fill);
        let mut x = rect.left() + padding.x;
        if let Some(icon) = icon {
            let icon = if dismiss.is_some() && hovered {
                Icon::X
            } else {
                icon
            };
            icon.image(color, icon_size).paint_at(ui, icon_rect);
            x += icon_width;
        }
        let pos = egui::pos2(x, rect.center().y - galley.size().y / 2.0);
        ui.painter().galley(pos, galley, color);
    }
    focus_ring(ui, &response);
    if let Some(dismiss) = dismiss {
        focus_ring(ui, &dismiss);
    }
    (response, dismissed)
}

/// An animated busy indicator paced independently of the graphics driver.
pub fn spinner(ui: &mut egui::Ui, size: f32, color: Color32) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    if ui.is_rect_visible(rect) {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(33));
        let radius = size / 2.0 - 2.0;
        let start = ui.input(|input| input.time) * std::f64::consts::TAU * 1.2;
        let sweep = 250_f64.to_radians();
        let points = (0..20)
            .map(|index| {
                let angle = start + sweep * f64::from(index) / 19.0;
                let (sin, cos) = angle.sin_cos();
                rect.center() + radius * egui::vec2(cos as f32, sin as f32)
            })
            .collect();
        ui.painter()
            .add(egui::Shape::line(points, Stroke::new(2.0, color)));
    }
    response
}

/// Truncated single-line text in a given font and colour.
pub fn text(
    ui: &mut egui::Ui,
    text: impl Into<String>,
    font: egui::FontId,
    color: Color32,
) -> Response {
    let text = text.into();
    if crate::bidi::is_rtl(&text) {
        // Laid out here so a cut lands at the reading end, on the left.
        let galley = crate::bidi::layout(
            ui.painter(),
            &text,
            font,
            color,
            ui.available_width(),
            1,
            Some(crate::bidi::ELLIPSIS),
        );
        return ui.add(egui::Label::new(galley).selectable(false));
    }
    ui.add(
        egui::Label::new(egui::RichText::new(text).font(font).color(color))
            .truncate()
            .selectable(false),
    )
}

/// Single-line text that acts like a link: underlines on hover, clickable.
pub fn link(
    ui: &mut egui::Ui,
    text: impl Into<String>,
    font: egui::FontId,
    color: Color32,
) -> Response {
    let text = text.into();
    let response = if crate::bidi::is_rtl(&text) {
        let galley = crate::bidi::layout(
            ui.painter(),
            &text,
            font,
            color,
            ui.available_width(),
            1,
            Some(crate::bidi::ELLIPSIS),
        );
        ui.add(
            egui::Label::new(galley)
                .selectable(false)
                .sense(Sense::click()),
        )
    } else {
        ui.add(
            egui::Label::new(egui::RichText::new(text.clone()).font(font).color(color))
                .truncate()
                .selectable(false)
                .sense(Sense::click()),
        )
    };
    response
        .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Link, ui.is_enabled(), &text));
    focus_ring(ui, &response);
    if response.hovered() {
        let rect = response.rect;
        ui.painter()
            .hline(rect.x_range(), rect.bottom() - 1.0, Stroke::new(1.0, color));
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

pub fn section_title(ui: &mut egui::Ui, palette: &Palette, label: &str) -> Response {
    text(ui, label, bold(17.0), palette.text)
}

pub fn subtle(ui: &mut egui::Ui, palette: &Palette, label: &str) -> Response {
    text(ui, label, regular(13.0), palette.secondary)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Palette files name the sixteen colours every app shares, and only
    /// those: a typo is an invalid file, not an ignored colour.
    #[test]
    fn palette_files_set_every_base_colour() {
        use fastframe_theme::Palette as _;
        for name in fastframe_theme::BASE_COLORS {
            let mut palette = Palette::dark();
            assert!(palette.set(name, Color32::from_rgb(1, 2, 3)), "{name}");
            assert_ne!(palette, Palette::dark(), "{name}");
        }
        assert!(!Palette::dark().set("typo", Color32::RED));
        let light: Palette =
            fastframe_theme::parse_palette(r##"{"base":"light","colors":{"accent":"#8c3fa5"}}"##)
                .unwrap();
        assert!(!light.dark);
        assert_eq!(light.accent, Color32::from_rgb(140, 63, 165));
        assert_eq!(light.window, Palette::light().window);
    }

    /// Compared line by line: a Windows checkout may turn the files' line
    /// endings into CRLF, which no Linux package ships.
    #[test]
    fn the_shipped_omarchy_files_are_the_shared_ones() {
        let lines = |text: &str| text.replace("\r\n", "\n");
        assert_eq!(
            lines(include_str!("../contrib/omarchy/spotifast-theme")),
            lines(&fastframe_theme::omarchy::hook_script("spotifast"))
        );
        assert_eq!(
            lines(include_str!("../contrib/omarchy/spotifast.json.tpl")),
            lines(fastframe_theme::omarchy::BASE_TEMPLATE)
        );
    }

    #[test]
    fn an_updater_trial_of_the_legacy_profile_leaves_the_desktop_alone() {
        assert!(!desktop_themes_allowed(
            &crate::paths::AppDirs::legacy().config.join("themes")
        ));
        assert!(desktop_themes_allowed(
            &crate::paths::AppDirs::discover().config.join("themes")
        ));
    }

    #[test]
    fn every_catalog_problem_has_a_sentence() {
        let catalog = Catalog::default();
        assert_eq!(catalog_detail(&catalog, Locale::English, None), "");
        assert!(
            catalog_detail(&catalog, Locale::English, Some("gone.json")).contains("last usable")
        );
    }

    /// A palette replaces egui's visuals, which must not take back the
    /// desktop's text rendering: on Linux, linear coverage in both themes.
    #[test]
    fn palettes_keep_the_desktops_text_rendering() {
        for palette in [Palette::dark(), Palette::light()] {
            let ctx = egui::Context::default();
            apply(&ctx, &palette);
            let options = ctx.global_style().visuals.text_options;
            assert_eq!(
                options.color_transfer_function,
                text_rendering().color_transfer_function(palette.dark),
                "dark: {}",
                palette.dark
            );
            if cfg!(target_os = "linux") {
                assert_eq!(
                    options.color_transfer_function,
                    egui::epaint::FontColorTransferFunction::Off
                );
            }
            assert!(options.subpixel_binning);
        }
    }

    /// Native desktop apps keep the arrow over buttons and switch to the
    /// hand only over links (#508).
    #[test]
    fn only_links_show_the_hand_cursor() {
        let ctx = egui::Context::default();
        install(&ctx);
        let palette = Palette::dark();
        let draw = |ctx: &egui::Context, pointer: Option<egui::Pos2>| {
            let mut rects = (egui::Rect::NOTHING, egui::Rect::NOTHING);
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(400.0, 200.0),
                )),
                events: pointer
                    .map(|at| vec![egui::Event::PointerMoved(at)])
                    .unwrap_or_default(),
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| {
                rects.0 = pill_button(ui, &palette, "Play", true).rect;
                rects.1 = link(ui, "Bonobo", regular(14.0), palette.text).rect;
            });
            output.textures_delta.clear();
            (rects, output.platform_output.cursor_icon)
        };
        draw(&ctx, None);
        let ((button, link_rect), _) = draw(&ctx, None);
        let (_, over_button) = draw(&ctx, Some(button.center()));
        assert_eq!(over_button, egui::CursorIcon::Default);
        let (_, over_link) = draw(&ctx, Some(link_rect.center()));
        assert_eq!(over_link, egui::CursorIcon::PointingHand);
    }

    #[test]
    fn a_local_palette_keeps_spotifasts_widget_style_local() {
        let ctx = egui::Context::default();
        apply(&ctx, &Palette::light());
        let dark = Palette::dark();
        let mut output = ctx.run_ui(Default::default(), |ui| {
            apply_local(ui, &dark);
            let style = ui.style();
            assert!(style.visuals.dark_mode);
            assert_eq!(
                style.visuals.widgets.inactive.corner_radius,
                CornerRadius::same(RADIUS_SMALL + 2)
            );
            assert_eq!(
                style.visuals.selection.stroke,
                Stroke::new(1.0, dark.accent)
            );
            assert_eq!(style.spacing.button_padding, Vec2::new(12.0, 6.0));
        });
        output.textures_delta.clear();
        assert!(!ctx.global_style().visuals.dark_mode);
    }

    #[test]
    fn fonts_install_and_layout_emojis() {
        let ctx = egui::Context::default();
        install(&ctx);
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let galley = ui.painter().layout_no_wrap(
                "Rosewood 🔥 Otomo 🎵 ❤️ 🚀".to_string(),
                regular(14.0),
                Color32::WHITE,
            );
            assert!(galley.rows[0].glyphs.len() >= 5);
        });
        output.textures_delta.clear();
    }

    /// The monochrome emoji face comes right after Inter at every weight
    /// and in the monospace family, ahead of egui's own emoji pair.
    #[test]
    fn the_emoji_face_follows_inter_everywhere() {
        let ctx = egui::Context::default();
        install(&ctx);
        ctx.run_ui(egui::RawInput::default(), |_| {})
            .textures_delta
            .clear();
        let fonts = ctx.fonts(|fonts| fonts.definitions().clone());
        for weight in fastframe_fonts::Weight::ALL {
            let family = &fonts.families[&weight.family()];
            assert_eq!(family[..2], [weight.name(), "noto_emoji"], "{weight:?}");
        }
        assert_eq!(
            fonts.families[&egui::FontFamily::Monospace][1],
            "noto_emoji"
        );
    }

    #[test]
    fn fonts_install_and_layout_mixed_cjk() {
        let ctx = egui::Context::default();
        install(&ctx);
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let galley = ui.painter().layout_no_wrap(
                "Track 87: 恋におちて -Fall in love- (Live)".to_string(),
                regular(14.0),
                Color32::WHITE,
            );
            assert!(!galley.rows.is_empty());
            assert!(galley.rows[0].glyphs.len() >= 10);
        });
        output.textures_delta.clear();
    }
}
