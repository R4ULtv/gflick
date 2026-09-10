//! Shared UI primitives. Containers use `min_w_0` so long text wraps in flex layouts.
use crate::theme::*;
use gpui_kit::base::StyledExt as _;
use gpui_kit::component::{Icon, Sizable as _};
use gpui_kit::{AnyElement, Div, Pixels, SharedString, div, prelude::*, px, rgb};

/// A panel for settings groups and info tables; it stays unclipped for focus rings.
pub fn card() -> Div {
    div()
        .min_w_0()
        .rounded_xl()
        .border_1()
        .border_color(rgb(LINE))
        .bg(rgb(SURFACE))
}

/// A card title block with a skimmable glyph and a rule below it.
pub fn card_header(
    icon: impl Into<Icon>,
    title: impl Into<SharedString>,
    description: &str,
) -> Div {
    header_frame().child(title_block(icon, title, description))
}

/// A card header with a control that selects what the body describes.
pub fn card_header_aside(
    icon: impl Into<Icon>,
    title: impl Into<SharedString>,
    description: &str,
    aside: impl IntoElement,
) -> Div {
    header_frame()
        .child(title_block(icon, title, description))
        .child(aside)
}

fn header_frame() -> Div {
    div()
        .min_w_0()
        .flex()
        .items_start()
        .justify_between()
        .gap_4()
        .px_5()
        .py_4()
        .border_b_1()
        .border_color(rgb(LINE))
}

fn title_block(icon: impl Into<Icon>, title: impl Into<SharedString>, description: &str) -> Div {
    div()
        .min_w_0()
        .flex_1()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .min_w_0()
                .flex()
                .items_center()
                .gap(px(7.0))
                .text_size(text::TITLE)
                .font_semibold()
                .text_color(rgb(TEXT))
                // Keep the glyph on the title line to avoid an unnecessary icon column.
                .child(
                    icon.into()
                        .with_size(px(15.0))
                        .flex_shrink_0()
                        .text_color(rgb(NOTICE_ICON)),
                )
                .child(div().min_w_0().child(title.into())),
        )
        .when(!description.is_empty(), |el| {
            el.child(
                div()
                    .text_size(text::TINY)
                    .text_color(rgb(MUTED_2))
                    .child(description.to_owned()),
            )
        })
}

/// A card's content area.
pub fn card_body() -> Div {
    div().min_w_0().px_5().py_1()
}

/// A small uppercase label introducing the thing below it.
pub fn eyebrow(label: impl Into<SharedString>) -> Div {
    div()
        .text_size(text::MICRO)
        .font_semibold()
        .text_color(rgb(ACCENT_TEXT))
        .child(label.into())
}

/// A static tag: a value that reads as metadata rather than a control.
pub fn chip(label: impl Into<SharedString>) -> Div {
    div()
        .px(px(6.0))
        .py(px(3.0))
        .rounded_md()
        .bg(rgb(SURFACE_3))
        .text_size(text::MICRO)
        .font_semibold()
        .text_color(rgb(MUTED))
        .child(label.into())
}

/// A chip carrying the accent, for the one value on a card worth pointing at.
pub fn chip_accent(label: impl Into<SharedString>) -> Div {
    chip(label).bg(rgb(ACCENT_MUTED)).text_color(rgb(0xc7d0ff))
}

/// A white count badge inside a filled accent button.
pub fn count_badge(count: usize) -> Div {
    div()
        .flex_shrink_0()
        // Sized rather than padded, so it reads as one square mark next to the
        // label instead of a pill that grew out of its digit.
        .h(px(17.0))
        .min_w(px(18.0))
        .px(px(4.0))
        .rounded(px(5.0))
        .bg(rgb(0xffffff))
        .flex()
        .items_center()
        .justify_center()
        .text_size(text::MICRO)
        .font_semibold()
        .text_color(rgb(ACCENT))
        .child(count.to_string())
}

/// A status light.
pub fn dot(color: u32) -> Div {
    div()
        .size(px(6.0))
        .flex_shrink_0()
        .rounded_full()
        .bg(rgb(color))
}

/// A recessed container, for controls grouped inside a card.
pub fn well() -> Div {
    div()
        .min_w_0()
        .rounded_lg()
        .border_1()
        .border_color(rgb(LINE))
        .bg(rgb(DEEP))
}

/// A card's settings, ruled off from each other.
///
/// The header already rules the first row off, so only the rows after it carry
/// a divider.
pub fn card_rows(rows: Vec<AnyElement>) -> Div {
    card_body().children(rows.into_iter().enumerate().map(|(index, row)| {
        div()
            .min_w_0()
            .when(index > 0, |el| el.border_t_1().border_color(rgb(LINE_SOFT)))
            .child(row)
    }))
}

/// One setting: its copy on the left, its control on the right.
pub fn setting_row() -> Div {
    div()
        .min_w_0()
        .flex()
        .items_center()
        .justify_between()
        .gap_5()
        .py_4()
}

/// Setting label/help text; `pending` marks an unapplied edit.
pub fn row_copy(label: impl Into<SharedString>, help: &str, pending: bool) -> Div {
    div()
        .min_w_0()
        .flex_1()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_size(text::BODY)
                .font_semibold()
                .text_color(rgb(TEXT))
                .child(label.into())
                .when(pending, |el| el.child(dot(ACCENT_TEXT))),
        )
        .when(!help.is_empty(), |el| {
            el.child(
                div()
                    .max_w(px(420.0))
                    .text_size(text::TINY)
                    .text_color(rgb(MUTED_2))
                    .child(help.to_owned()),
            )
        })
}

/// An unboxed label/value row, separated from adjacent rows by a hairline.
pub fn info_row(label: &'static str, value: impl Into<SharedString>, divided: bool) -> Div {
    div()
        .min_w_0()
        .flex()
        .items_baseline()
        .justify_between()
        .gap_4()
        .py(px(11.0))
        .when(divided, |el| el.border_t_1().border_color(rgb(LINE_SOFT)))
        .child(
            div()
                .flex_shrink_0()
                .text_size(text::SMALL)
                .text_color(rgb(MUTED_2))
                .child(label),
        )
        .child(
            div()
                .min_w_0()
                .text_size(text::SMALL)
                .text_color(rgb(TEXT))
                .text_right()
                .child(value.into()),
        )
}

/// An info table: rows of label/value pairs, ruled off from one another.
pub fn info_table(rows: Vec<(&'static str, SharedString)>) -> Div {
    div().min_w_0().flex().flex_col().children(
        rows.into_iter()
            .enumerate()
            .map(|(index, (label, value))| info_row(label, value, index > 0)),
    )
}

/// A button label that keeps the page's text scale instead of Kit's 16px default.
pub fn button_label(label: impl Into<SharedString>, size: Pixels) -> Div {
    div().text_size(size).child(label.into())
}

/// A card aside whose `tone` colors only the attention-carrying glyph.
pub fn notice(icon: impl Into<Icon>, tone: u32, body: impl Into<SharedString>) -> Div {
    div()
        .min_w_0()
        .flex()
        .items_start()
        .gap(px(9.0))
        .p_3()
        .rounded_lg()
        .bg(rgb(SURFACE_2))
        .text_size(text::TINY)
        .text_color(rgb(MUTED))
        .child(
            div()
                .size(px(19.0))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(5.0))
                .bg(rgb(SURFACE_3))
                .child(icon.into().with_size(px(12.0)).text_color(rgb(tone))),
        )
        .child(div().min_w_0().child(body.into()))
}

/// Derived values shown as cells sharing one border beneath their control.
pub fn readout(cells: Vec<(&'static str, SharedString, u32)>) -> Div {
    let last = cells.len().saturating_sub(1);
    div()
        .min_w_0()
        .flex()
        .rounded_lg()
        .border_1()
        .border_color(rgb(LINE))
        .overflow_hidden()
        .children(
            cells
                .into_iter()
                .enumerate()
                .map(|(index, (label, value, tone))| {
                    div()
                        .min_w_0()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .px_3()
                        .py(px(11.0))
                        .bg(rgb(DEEP))
                        .when(index < last, |el| el.border_r_1().border_color(rgb(LINE)))
                        .child(caption(label))
                        .child(
                            div()
                                .text_size(text::SMALL)
                                .font_semibold()
                                .text_color(rgb(tone))
                                .child(value),
                        )
                }),
        )
}

/// A caption above a group of controls inside a card.
pub fn caption(label: impl Into<SharedString>) -> Div {
    div()
        .text_size(text::MICRO)
        .font_semibold()
        .text_color(rgb(MUTED_2))
        .child(label.into())
}
