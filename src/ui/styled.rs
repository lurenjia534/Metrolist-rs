//! Shared gpui-component surfaces so pages can match official Longbridge examples
//! instead of stacking ad-hoc `div`s.
//!
//! Colors, radii, and type sizes come from `cx.theme()` / semantic typography
//! tokens. Callers attach behavior (`on_click`, route changes) after the helper
//! returns a `Button`, `GroupBox`, or `Div`.

use gpui::prelude::FluentBuilder as _;
use gpui::{App, Div, ElementId, ParentElement, SharedString, StyleRefinement, Styled, div, px};
use gpui_component::{
    ActiveTheme, Icon, IconName, Selectable, Sizable, StyledExt,
    alert::Alert,
    button::{Button, ButtonVariants},
    group_box::{GroupBox, GroupBoxVariants},
    h_flex,
    skeleton::Skeleton,
    spinner::Spinner,
    v_flex,
};

/// Large page title plus muted description, matching official gallery headers.
pub fn page_header(
    title: impl Into<SharedString>,
    subtitle: impl Into<SharedString>,
    cx: &App,
) -> Div {
    let typography = cx.theme().semantic_tokens().typography;
    let subtitle = subtitle.into();
    v_flex()
        .flex_shrink_0()
        .w_full()
        .min_w_0()
        .gap_1()
        .child(
            div()
                .text_size(typography.xl.size)
                .line_height(typography.xl.line_height)
                .font_semibold()
                .text_color(cx.theme().foreground)
                .child(title.into()),
        )
        .when(!subtitle.is_empty(), |header| {
            header.child(
                div()
                    .text_size(typography.sm.size)
                    .line_height(typography.sm.line_height)
                    .text_color(cx.theme().muted_foreground)
                    .child(subtitle),
            )
        })
}

/// Title column used by section rows and existing `section_heading` call sites.
pub fn section_heading(
    title: impl Into<SharedString>,
    subtitle: Option<SharedString>,
    cx: &App,
) -> Div {
    let typography = cx.theme().semantic_tokens().typography;
    v_flex()
        .flex_1()
        .min_w_0()
        .gap_1()
        .child(
            div()
                .text_size(typography.lg.size)
                .line_height(typography.lg.line_height)
                .font_semibold()
                .text_color(cx.theme().foreground)
                .child(title.into()),
        )
        .when_some(subtitle, |column, subtitle| {
            column.child(
                div()
                    .text_size(typography.sm.size)
                    .line_height(typography.sm.line_height)
                    .text_color(cx.theme().muted_foreground)
                    .child(subtitle),
            )
        })
}

/// Section title row with an optional trailing [`Button`] (`See all`, `Play all`).
pub fn section_header(
    title: impl Into<SharedString>,
    subtitle: Option<SharedString>,
    trailing_action: Option<Button>,
    cx: &App,
) -> Div {
    h_flex()
        .w_full()
        .items_start()
        .justify_between()
        .gap_3()
        .child(section_heading(title, subtitle, cx))
        .when_some(trailing_action, |row, action| {
            row.child(action.flex_shrink_0())
        })
}

/// Centered empty placeholder: icon, copy, and an optional action button.
pub fn empty_state(
    icon: IconName,
    title: impl Into<SharedString>,
    detail: impl Into<SharedString>,
    action: Option<Button>,
    cx: &App,
) -> Div {
    let typography = cx.theme().semantic_tokens().typography;
    v_flex()
        .w_full()
        .items_center()
        .justify_center()
        .gap_3()
        .py_8()
        .px_4()
        .child(
            div()
                .size_12()
                .flex()
                .items_center()
                .justify_center()
                .rounded(cx.theme().radius_lg)
                .bg(cx.theme().secondary)
                .text_color(cx.theme().muted_foreground)
                .child(Icon::new(icon).large()),
        )
        .child(
            div()
                .text_size(typography.lg.size)
                .line_height(typography.lg.line_height)
                .font_semibold()
                .text_color(cx.theme().foreground)
                .text_center()
                .child(title.into()),
        )
        .child(
            div()
                .max_w(px(420.))
                .text_size(typography.sm.size)
                .line_height(typography.sm.line_height)
                .text_color(cx.theme().muted_foreground)
                .text_center()
                .child(detail.into()),
        )
        .when_some(action, |column, action| column.child(action))
}

/// Error callout using [`Alert::error`], with an optional retry button below.
pub fn error_state(
    title: impl Into<SharedString>,
    message: impl Into<SharedString>,
    retry: Option<Button>,
    cx: &App,
) -> Div {
    let title = title.into();
    let message = message.into();
    v_flex()
        .w_full()
        .gap_3()
        .child(
            Alert::error(SharedString::from(format!("error-state-{title}")), message)
                .title(title)
                .rounded(cx.theme().radius_lg),
        )
        .when_some(retry, |column, retry| column.child(retry))
}

/// Spinner plus skeleton placeholders — not a raw `LoaderCircle` and string.
pub fn loading_state(label: impl Into<SharedString>, cx: &App) -> Div {
    let typography = cx.theme().semantic_tokens().typography;
    let radius = cx.theme().radius;
    let radius_lg = cx.theme().radius_lg;
    v_flex()
        .w_full()
        .gap_3()
        .child(
            h_flex()
                .items_center()
                .gap_2()
                .child(
                    Spinner::new()
                        .icon(IconName::LoaderCircle)
                        .color(cx.theme().primary),
                )
                .child(
                    div()
                        .text_size(typography.sm.size)
                        .line_height(typography.sm.line_height)
                        .text_color(cx.theme().muted_foreground)
                        .child(label.into()),
                ),
        )
        .child(h_flex().w_full().gap_3().children((0..3).map(|_| {
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_2()
                .child(Skeleton::new().w_full().h(px(112.)).rounded(radius_lg))
                .child(Skeleton::new().w_full().h_4().rounded(radius))
                .child(Skeleton::new().secondary().w(px(96.)).h_3().rounded(radius))
        })))
        .child(v_flex().w_full().gap_2().children((0..2).map(|_| {
            h_flex()
                .w_full()
                .items_center()
                .gap_3()
                .child(Skeleton::new().size(px(40.)).rounded(radius))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .child(Skeleton::new().w_full().h_4().rounded(radius))
                        .child(
                            Skeleton::new()
                                .secondary()
                                .w(px(160.))
                                .h_3()
                                .rounded(radius),
                        ),
                )
        })))
}

fn card_content_style(cx: &App) -> StyleRefinement {
    StyleRefinement::default().rounded(cx.theme().radius_lg)
}

/// Filled [`GroupBox`] surface for settings blocks and featured panels.
pub fn surface_card(cx: &App) -> GroupBox {
    GroupBox::new()
        .fill()
        .rounded(cx.theme().radius_lg)
        .content_style(card_content_style(cx))
}

/// Outlined [`GroupBox`] for secondary groupings.
pub fn outlined_card(cx: &App) -> GroupBox {
    GroupBox::new()
        .outline()
        .rounded(cx.theme().radius_lg)
        .content_style(card_content_style(cx))
}

/// Compact selectable filter chip. Attach `.on_click` at the call site.
pub fn filter_chip(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    selected: bool,
) -> Button {
    Button::new(id)
        .ghost()
        .compact()
        .small()
        .label(label)
        .selected(selected)
}

/// Compact ghost icon button with a tooltip.
pub fn icon_action(
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

/// Existing helper name used throughout the shell. Prefer [`icon_action`].
pub fn icon_ghost_button(
    id: impl Into<ElementId>,
    icon: IconName,
    tooltip: impl Into<SharedString>,
) -> Button {
    icon_action(id, icon, tooltip)
}
