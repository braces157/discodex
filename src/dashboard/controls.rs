use gpui::{Context, FontWeight, IntoElement, div, prelude::*, px, rgb, rgba};

use super::{
    Action, Dashboard,
    theme::{
        ACCENT_AMBER, ACCENT_AMBER_BORDER, ACCENT_AMBER_SOFT, ACCENT_LIME, ACCENT_LIME_BORDER,
        ACCENT_LIME_SOFT, BG_ELEVATED, BG_HOVER, BG_RECESSED, BG_SURFACE, BORDER_STRONG,
        BORDER_SUBTLE, FONT_ICON, FONT_MONO, TEXT_MUTED, TEXT_PRIMARY, TEXT_SECONDARY,
    },
};
use crate::usage::UsagePeriod;

pub fn icon_button(
    id: &'static str,
    glyph: &'static str,
    active: bool,
    action: Action,
    cx: &mut Context<Dashboard>,
) -> impl IntoElement + use<> {
    div()
        .id(id)
        .size(px(28.))
        .rounded(px(7.))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .font_family(FONT_ICON)
        .text_size(px(12.))
        .border_1()
        .border_color(rgba(if active {
            ACCENT_LIME_BORDER
        } else {
            0x00000000
        }))
        .bg(rgba(if active { ACCENT_LIME_SOFT } else { 0x00000000 }))
        .text_color(rgb(if active { ACCENT_LIME } else { TEXT_SECONDARY }))
        .hover(move |style| {
            style
                .bg(rgba(if active { ACCENT_LIME_SOFT } else { BG_HOVER }))
                .text_color(rgb(if active { ACCENT_LIME } else { TEXT_PRIMARY }))
        })
        .on_click(cx.listener(move |this, _, window, cx| {
            this.act_with_window(action.clone(), window, cx);
        }))
        .child(glyph)
}

pub fn icon_label_button(
    id: &'static str,
    glyph: &'static str,
    label: &'static str,
    active: bool,
    action: Action,
    cx: &mut Context<Dashboard>,
) -> impl IntoElement + use<> {
    div()
        .id(id)
        .h(px(28.))
        .px(px(8.))
        .rounded(px(7.))
        .flex()
        .items_center()
        .gap(px(5.))
        .cursor_pointer()
        .border_1()
        .border_color(rgba(if active {
            ACCENT_LIME_BORDER
        } else {
            0x00000000
        }))
        .bg(rgba(if active { ACCENT_LIME_SOFT } else { 0x00000000 }))
        .text_color(rgb(if active { ACCENT_LIME } else { TEXT_SECONDARY }))
        .hover(move |style| {
            style
                .bg(rgba(if active { ACCENT_LIME_SOFT } else { BG_HOVER }))
                .text_color(rgb(if active { ACCENT_LIME } else { TEXT_PRIMARY }))
        })
        .on_click(cx.listener(move |this, _, window, cx| {
            this.act_with_window(action.clone(), window, cx);
        }))
        .child(div().font_family(FONT_ICON).text_size(px(11.)).child(glyph))
        .child(
            div()
                .text_size(px(11.))
                .font_weight(FontWeight::MEDIUM)
                .child(label),
        )
}

pub fn segment_button<T: Into<gpui::ElementId>, U: Into<gpui::SharedString>>(
    id: T,
    label: U,
    selected: bool,
    accent: Option<u32>,
    action: Action,
    cx: &mut Context<Dashboard>,
) -> impl IntoElement + use<T, U> {
    let selected_text = accent.unwrap_or(TEXT_PRIMARY);
    div()
        .id(id)
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .py(px(5.))
        .px(px(8.))
        .rounded(px(6.))
        .cursor_pointer()
        .text_size(px(12.))
        .whitespace_nowrap()
        .border_1()
        .border_color(rgba(if selected { BORDER_STRONG } else { 0x00000000 }))
        .bg(if selected {
            rgba((BG_ELEVATED << 8) | 0xFF)
        } else {
            rgba(0x00000000)
        })
        .text_color(rgb(if selected {
            selected_text
        } else {
            TEXT_SECONDARY
        }))
        .when(selected, |this| this.font_weight(FontWeight::MEDIUM))
        .hover(move |style| {
            let style = if selected {
                style
            } else {
                style.bg(rgba(BG_HOVER))
            };
            style.text_color(rgb(if selected {
                selected_text
            } else {
                TEXT_PRIMARY
            }))
        })
        .on_click(cx.listener(move |this, _, _, cx| this.act(action.clone(), cx)))
        .child(label.into())
}

pub fn chip_button<T: Into<gpui::ElementId>, U: Into<gpui::SharedString>>(
    id: T,
    label: U,
    selected: bool,
    action: Action,
    cx: &mut Context<Dashboard>,
) -> impl IntoElement + use<T, U> {
    div()
        .id(id)
        .px(px(8.))
        .py(px(3.))
        .rounded(px(6.))
        .cursor_pointer()
        .text_size(px(11.))
        .border_1()
        .border_color(rgba(if selected {
            ACCENT_LIME_BORDER
        } else {
            BORDER_SUBTLE
        }))
        .bg(if selected {
            rgba(ACCENT_LIME_SOFT)
        } else {
            rgba(0x00000000)
        })
        .text_color(rgb(if selected {
            ACCENT_LIME
        } else {
            TEXT_SECONDARY
        }))
        .when(selected, |this| this.font_weight(FontWeight::MEDIUM))
        .hover(move |style| {
            let style = if selected {
                style
            } else {
                style.bg(rgba(BG_HOVER))
            };
            style.text_color(rgb(if selected { ACCENT_LIME } else { TEXT_PRIMARY }))
        })
        .on_click(cx.listener(move |this, _, _, cx| this.act(action.clone(), cx)))
        .child(label.into())
}

pub fn footer_button(
    id: &'static str,
    glyph: &'static str,
    label: &'static str,
    active: bool,
    action: Action,
    cx: &mut Context<Dashboard>,
) -> impl IntoElement + use<> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(5.))
        .px(px(8.))
        .py(px(4.))
        .rounded(px(6.))
        .cursor_pointer()
        .border_1()
        .border_color(rgba(if active {
            ACCENT_LIME_BORDER
        } else {
            0x00000000
        }))
        .bg(rgba(if active { ACCENT_LIME_SOFT } else { 0x00000000 }))
        .text_color(rgb(if active { ACCENT_LIME } else { TEXT_SECONDARY }))
        .hover(move |style| {
            style
                .bg(rgba(if active { ACCENT_LIME_SOFT } else { BG_HOVER }))
                .text_color(rgb(if active { ACCENT_LIME } else { TEXT_PRIMARY }))
        })
        .on_click(cx.listener(move |this, _, _, cx| this.act(action.clone(), cx)))
        .child(div().font_family(FONT_ICON).text_size(px(11.)).child(glyph))
        .child(div().text_size(px(11.)).child(label))
}

pub fn render_empty_state(
    period: UsagePeriod,
    cx: &mut Context<Dashboard>,
) -> impl IntoElement + use<> {
    let heading = match period {
        UsagePeriod::Last24h => "No activity in the last 24 hours",
        UsagePeriod::Week => "No activity in the last 7 days",
        UsagePeriod::Month => "No activity in the last 30 days",
        UsagePeriod::All => "No recorded usage yet",
    };
    let suggestions: Vec<(&'static str, UsagePeriod)> = match period {
        UsagePeriod::Last24h => vec![
            ("View 7 days", UsagePeriod::Week),
            ("View 30 days", UsagePeriod::Month),
            ("View All", UsagePeriod::All),
        ],
        UsagePeriod::Week => vec![
            ("View 30 days", UsagePeriod::Month),
            ("View All", UsagePeriod::All),
        ],
        UsagePeriod::Month => vec![("View All", UsagePeriod::All)],
        UsagePeriod::All => Vec::new(),
    };

    div()
        .bg(rgb(BG_SURFACE))
        .border_1()
        .border_color(rgba(BORDER_SUBTLE))
        .rounded(px(12.))
        .py(px(28.))
        .px(px(20.))
        .flex()
        .flex_col()
        .items_center()
        .child(
            div()
                .size(px(36.))
                .rounded_full()
                .bg(rgb(BG_ELEVATED))
                .border_1()
                .border_color(rgba(BORDER_SUBTLE))
                .flex()
                .items_center()
                .justify_center()
                .font_family(FONT_ICON)
                .text_size(px(15.))
                .text_color(rgb(ACCENT_LIME))
                .child("\u{E9D9}"),
        )
        .child(
            div()
                .mt(px(12.))
                .text_size(px(13.5))
                .font_weight(FontWeight::MEDIUM)
                .text_color(rgb(TEXT_PRIMARY))
                .child(heading),
        )
        .child(
            div()
                .mt(px(4.))
                .text_size(px(11.5))
                .text_color(rgb(TEXT_MUTED))
                .text_center()
                .child("Supported local coding agent sessions appear here automatically."),
        )
        .when(!suggestions.is_empty(), |this| {
            let mut jump_chips = Vec::with_capacity(suggestions.len());
            for (idx, (label, target)) in suggestions.into_iter().enumerate() {
                jump_chips.push(chip_button(
                    ("empty-jump", idx),
                    label,
                    false,
                    Action::Period(target),
                    cx,
                ));
            }
            this.child(div().mt(px(14.)).flex().gap(px(6.)).children(jump_chips))
        })
}

pub fn render_notice_pill(
    warnings: &[String],
    info_open: bool,
    cx: &mut Context<Dashboard>,
) -> impl IntoElement + use<> {
    let label = if warnings.len() == 1 {
        "Approximate base-rate estimate".to_string()
    } else {
        format!("Partial data · {} notes", warnings.len())
    };
    div()
        .id("notice-toggle")
        .bg(rgba(ACCENT_AMBER_SOFT))
        .border_1()
        .border_color(rgba(ACCENT_AMBER_BORDER))
        .rounded(px(8.))
        .px(px(10.))
        .py(px(7.))
        .cursor_pointer()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(8.))
        .on_click(cx.listener(|this, _, _, cx| this.act(Action::Info, cx)))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(7.))
                .min_w_0()
                .child(
                    div()
                        .font_family(FONT_ICON)
                        .text_size(px(12.))
                        .text_color(rgb(ACCENT_AMBER))
                        .child("\u{E7BA}"),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(rgb(ACCENT_AMBER))
                        .truncate()
                        .child(label),
                ),
        )
        .child(
            div()
                .flex_shrink_0()
                .text_size(px(10.5))
                .font_weight(FontWeight::MEDIUM)
                .text_color(rgb(ACCENT_AMBER))
                .child(if info_open { "Hide" } else { "Details" }),
        )
}

pub fn render_info_drawer(date: &str, warnings: Vec<String>) -> impl IntoElement + use<> {
    div()
        .bg(rgb(BG_SURFACE))
        .border_1()
        .border_color(rgba(BORDER_SUBTLE))
        .rounded(px(10.))
        .p(px(12.))
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_size(px(11.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(TEXT_PRIMARY))
                        .child("Pricing & Notes"),
                )
                .child(
                    div()
                        .font_family(FONT_MONO)
                        .text_size(px(10.))
                        .text_color(rgb(TEXT_MUTED))
                        .child(format!("Checked {date}")),
                ),
        )
        .child(
            div()
                .text_size(px(11.))
                .text_color(rgb(TEXT_SECONDARY))
                .child("Base-rate API approximation using official standard rates (not a subscription bill). Custom entries in usage-rates.toml override the catalog."),
        )
        .child(
            div()
                .font_family(FONT_MONO)
                .text_size(px(10.))
                .text_color(rgb(TEXT_MUTED))
                .child("developers.openai.com/api/docs/pricing · platform.claude.com/docs/en/about-claude/pricing · ai.google.dev/gemini-api/docs/pricing · api-docs.deepseek.com/quick_start/pricing"),
        )
        .child(
            div()
                .text_size(px(11.))
                .text_color(rgb(TEXT_MUTED))
                .child("Calls count recorded observations. Cached input is part of input; reasoning is part of output. Cache Saved estimates avoided input charges from cached reads. An asterisk (*) marks a partial total."),
        )
        .children(warnings.into_iter().map(|warning| {
            div()
                .text_size(px(11.))
                .text_color(rgb(ACCENT_AMBER))
                .child(format!("· {warning}"))
        }))
}

pub fn render_footer(
    updated: &str,
    scanned_files: usize,
    info_open: bool,
    cx: &mut Context<Dashboard>,
) -> impl IntoElement + use<> {
    div()
        .flex_none()
        .bg(rgb(BG_RECESSED))
        .border_t_1()
        .border_color(rgba(BORDER_SUBTLE))
        .px(px(16.))
        .py(px(9.))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(8.))
        .child(
            div()
                .font_family(FONT_MONO)
                .text_size(px(10.5))
                .text_color(rgb(TEXT_MUTED))
                .truncate()
                .child(format!("{updated} · {scanned_files} files")),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(4.))
                .flex_shrink_0()
                .child(footer_button(
                    "info",
                    "\u{E946}",
                    "Info",
                    info_open,
                    Action::Info,
                    cx,
                ))
                .child(footer_button(
                    "rates",
                    "\u{E70F}",
                    "Edit rates",
                    false,
                    Action::Rates,
                    cx,
                )),
        )
}
