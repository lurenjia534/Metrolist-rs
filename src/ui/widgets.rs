use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    ActiveTheme, Icon, IconName, StyledExt,
    button::{Button, ButtonVariants},
    h_flex, v_flex,
};

pub const MEDIA_TILE_WIDTH: Pixels = px(176.);
pub const MEDIA_TILE_MIN_WIDTH: Pixels = px(156.);
pub const MEDIA_TILE_MAX_WIDTH: Pixels = px(196.);

pub fn truncated_line(text: impl Into<SharedString>) -> Div {
    div()
        .w_full()
        .min_w_0()
        .overflow_hidden()
        .truncate()
        .child(text.into())
}

pub fn title_line(text: impl Into<SharedString>) -> Div {
    truncated_line(text).font_medium()
}

pub fn subtitle_line(text: impl Into<SharedString>, color: Hsla) -> Div {
    truncated_line(text).text_sm().text_color(color)
}

pub fn caption_line(text: impl Into<SharedString>, color: Hsla) -> Div {
    truncated_line(text).text_xs().text_color(color)
}

pub fn media_text_block(
    title: impl Into<SharedString>,
    subtitle: impl Into<SharedString>,
    extra: Option<SharedString>,
    cx: &App,
) -> Div {
    let muted = cx.theme().muted_foreground;
    v_flex()
        .flex_1()
        .min_w_0()
        .gap_0()
        .child(title_line(title))
        .child(subtitle_line(subtitle, muted))
        .when_some(extra, |column, extra| {
            column.child(caption_line(extra, muted))
        })
}

pub fn icon_ghost_button(
    id: impl Into<ElementId>,
    icon: IconName,
    tooltip: impl Into<SharedString>,
) -> Button {
    Button::new(id)
        .ghost()
        .compact()
        .icon(icon)
        .tooltip(tooltip)
}

pub fn tile_row() -> Div {
    h_flex().w_full().flex_wrap().items_start().gap_3()
}

pub fn media_tile_shell(id: impl Into<ElementId>, cx: &App) -> Stateful<Div> {
    v_flex()
        .id(id)
        .w(MEDIA_TILE_WIDTH)
        .min_w(MEDIA_TILE_MIN_WIDTH)
        .max_w(MEDIA_TILE_MAX_WIDTH)
        .gap_2()
        .rounded(cx.theme().radius_lg)
        .p_2()
        .cursor_pointer()
        .hover(|style| style.bg(cx.theme().secondary))
}

pub fn cover_frame(cx: &App) -> Div {
    div()
        .relative()
        .w_full()
        .aspect_square()
        .overflow_hidden()
        .rounded(cx.theme().radius)
        .bg(cx.theme().muted)
        .flex_shrink_0()
}

pub fn cover_play_badge(cx: &App) -> Div {
    div()
        .absolute()
        .right(px(8.))
        .bottom(px(8.))
        .size_8()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(999.))
        .bg(cx.theme().primary)
        .text_color(cx.theme().primary_foreground)
        .child(Icon::new(IconName::Play).size_4())
}

pub fn list_row_shell(id: impl Into<ElementId>, cx: &App) -> Stateful<Div> {
    h_flex()
        .id(id)
        .w_full()
        .min_w_0()
        .gap_3()
        .items_center()
        .rounded(cx.theme().radius)
        .px_2()
        .py_1()
        .hover(|style| style.bg(cx.theme().secondary))
}

pub fn featured_card_shell(cx: &App) -> Div {
    h_flex()
        .w_full()
        .max_w(px(720.))
        .min_w_0()
        .gap_4()
        .items_center()
        .rounded(cx.theme().radius_lg)
        .bg(cx.theme().secondary)
        .p_4()
}

pub fn section_heading(
    title: impl Into<SharedString>,
    subtitle: Option<SharedString>,
    cx: &App,
) -> Div {
    v_flex()
        .flex_1()
        .gap_1()
        .child(div().text_lg().font_semibold().child(title.into()))
        .when_some(subtitle, |column, subtitle| {
            column.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(subtitle),
            )
        })
}
