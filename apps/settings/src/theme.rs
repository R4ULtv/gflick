//! GFlick's palette and its GPUI Kit mapping, based on the settings-studio design.
use gpui_kit::component::{Theme, ThemeMode, ThemeTokens};
use gpui_kit::{App, Hsla, px, rgb, transparent_black};

/// Window and content background.
pub const BG: u32 = 0x262933;
/// Cards and raised panels.
pub const SURFACE: u32 = 0x2d303b;
/// Controls resting on a card.
pub const SURFACE_2: u32 = 0x323641;
/// The selected segment of a control, and card hover.
pub const SURFACE_3: u32 = 0x393d49;
/// Sidebar, inputs, and inset wells — anything that reads as recessed.
pub const DEEP: u32 = 0x20232b;
pub const ACCENT: u32 = 0x3450d1;
pub const ACCENT_HOVER: u32 = 0x405ddd;
/// Accent at panel weight, for tinted chips rather than filled controls.
pub const ACCENT_MUTED: u32 = 0x2c3d89;
/// Accent light enough to read as text on a dark panel.
pub const ACCENT_TEXT: u32 = 0x8f9fe8;
pub const TEXT: u32 = 0xf7f8fb;
/// Body copy and control labels.
pub const MUTED: u32 = 0xa5abba;
/// Captions, units, and help text — the quietest readable step.
pub const MUTED_2: u32 = 0x8b92a3;
pub const LINE: u32 = 0x404450;
/// The hairline that divides rows inside a card. Quieter than `LINE`, which
/// draws a card's own edge — a row divider only has to be followed, not seen.
pub const LINE_SOFT: u32 = 0x383c47;
pub const LINE_STRONG: u32 = 0x4b5060;
pub const SUCCESS: u32 = 0x57c490;
pub const WARNING: u32 = 0xe7ad58;
pub const DANGER: u32 = 0xe86d78;
/// A glyph that only marks where a sentence starts: bright enough to find,
/// quiet enough not to be read as a warning.
pub const NOTICE_ICON: u32 = 0xd9deea;
/// Focus ring. Bright enough to clear the accent it often sits next to.
pub const RING: u32 = 0x7890ff;

/// Text sizes, in points. The design is denser than Kit's 14/16 default scale,
/// so the steps are named here rather than reached for as raw numbers.
pub mod text {
    use gpui_kit::{Pixels, px};

    /// Uppercase eyebrows and units, sized for short labels without letter spacing.
    pub const MICRO: Pixels = px(10.0);
    /// Help text and chips.
    pub const TINY: Pixels = px(11.0);
    /// Captions and secondary values.
    pub const SMALL: Pixels = px(12.0);
    /// Control labels and values.
    pub const BODY: Pixels = px(13.0);
    /// Card titles.
    pub const TITLE: Pixels = px(15.0);
    /// The device name at the top of a page.
    pub const DISPLAY: Pixels = px(30.0);
}

fn color(hex: u32) -> Hsla {
    rgb(hex).into()
}

/// Switch to dark mode and replace Kit's palette with GFlick's.
pub fn apply(cx: &mut App) {
    Theme::change(ThemeMode::Dark, None, cx);

    let theme = Theme::global_mut(cx);
    theme.radius = px(8.0);
    theme.radius_lg = px(12.0);

    let colors = &mut theme.colors;
    colors.background = color(BG);
    colors.foreground = color(TEXT);
    colors.border = color(LINE);
    colors.ring = color(RING);
    colors.caret = color(TEXT);
    colors.selection = color(ACCENT_MUTED);

    // Inputs read as recessed wells, so they carry the strong line rather than
    // the panel line that divides equals.
    colors.input = color(LINE_STRONG);

    colors.primary = color(ACCENT);
    colors.primary_hover = color(ACCENT_HOVER);
    colors.primary_active = color(ACCENT_MUTED);
    colors.primary_foreground = color(0xffffff);

    colors.secondary = color(SURFACE_3);
    colors.secondary_hover = color(0x414653);
    colors.secondary_active = color(SURFACE_2);
    colors.secondary_foreground = color(TEXT);

    // `accent` is Kit's hover wash, not our brand accent.
    colors.accent = color(SURFACE_2);
    colors.accent_foreground = color(TEXT);
    colors.muted = color(SURFACE_2);
    colors.muted_foreground = color(MUTED);

    colors.success = color(SUCCESS);
    colors.success_hover = color(0x67ce9c);
    colors.success_active = color(0x4bb180);
    colors.success_foreground = color(0x14251d);
    colors.warning = color(WARNING);
    colors.warning_hover = color(0xefba6b);
    colors.warning_active = color(0xd69c4c);
    colors.warning_foreground = color(0x2a2013);
    colors.danger = color(DANGER);
    colors.danger_hover = color(0xef7d87);
    colors.danger_active = color(0xd45f6a);
    colors.danger_foreground = color(0xffffff);

    // The default button variant: a control on a card, not a bare label.
    colors.button = color(SURFACE_2);
    colors.button_hover = color(SURFACE_3);
    colors.button_active = color(0x414653);
    colors.button_foreground = color(TEXT);
    colors.button_primary = colors.primary;
    colors.button_primary_hover = colors.primary_hover;
    colors.button_primary_active = colors.primary_active;
    colors.button_primary_foreground = colors.primary_foreground;
    colors.button_secondary = colors.secondary;
    colors.button_secondary_hover = colors.secondary_hover;
    colors.button_secondary_active = colors.secondary_active;
    colors.button_secondary_foreground = colors.secondary_foreground;
    colors.button_danger = colors.danger;
    colors.button_danger_hover = colors.danger_hover;
    colors.button_danger_active = colors.danger_active;
    colors.button_danger_foreground = colors.danger_foreground;
    colors.button_success = colors.success;
    colors.button_success_hover = colors.success_hover;
    colors.button_success_active = colors.success_active;
    colors.button_success_foreground = colors.success_foreground;
    colors.button_warning = colors.warning;
    colors.button_warning_hover = colors.warning_hover;
    colors.button_warning_active = colors.warning_active;
    colors.button_warning_foreground = colors.warning_foreground;

    colors.sidebar = color(DEEP);
    colors.sidebar_border = color(0x353944);
    colors.sidebar_foreground = color(TEXT);
    colors.sidebar_accent = color(0x292d36);
    colors.sidebar_accent_foreground = color(TEXT);
    colors.sidebar_primary = colors.primary;
    colors.sidebar_primary_foreground = colors.primary_foreground;

    colors.popover = color(SURFACE);
    colors.popover_foreground = color(TEXT);
    colors.group_box = color(SURFACE);
    colors.group_box_foreground = color(TEXT);
    colors.title_bar = color(DEEP);
    colors.title_bar_border = color(0x353944);
    colors.status_bar = color(DEEP);
    colors.status_bar_border = color(0x353944);
    colors.accordion = color(SURFACE);
    colors.tiles = color(DEEP);

    // Tabs sit directly on the content background, underlined by the accent.
    colors.tab = transparent_black();
    colors.tab_bar = transparent_black();
    colors.tab_bar_segmented = color(DEEP);
    colors.tab_active = color(BG);
    colors.tab_active_foreground = color(TEXT);
    colors.tab_foreground = color(MUTED);

    colors.list = color(SURFACE);
    colors.list_hover = color(SURFACE_2);
    colors.list_active = color(0x33394b);
    colors.list_active_border = color(ACCENT);
    colors.list_head = color(DEEP);
    colors.list_even = color(SURFACE_2);

    colors.scrollbar = transparent_black();
    colors.scrollbar_thumb = color(LINE_STRONG);
    colors.scrollbar_thumb_hover = color(0x5b6172);

    colors.slider_bar = color(ACCENT);
    colors.slider_thumb = color(0xffffff);
    colors.switch = color(0x515664);
    colors.switch_thumb = color(0xffffff);
    colors.progress_bar = color(ACCENT);
    colors.skeleton = color(SURFACE_2);
    colors.description_list_label = color(DEEP);
    colors.description_list_label_foreground = color(MUTED);

    // `tokens` is the resolved copy the components actually read, and
    // `sync_base` pushes the result down to the layer that paints scrollbars.
    theme.tokens = ThemeTokens::from(&theme.colors);
    Theme::sync_base(cx);
}
