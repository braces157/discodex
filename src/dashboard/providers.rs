use gpui::{
    Context, FontWeight, IntoElement, ObjectFit, div, img, prelude::*, px, relative, rgb, rgba,
};

use super::{
    Action, Dashboard,
    controls::chip_button,
    format::ProviderShare,
    theme::{
        Assets, BG_ELEVATED, BG_HOVER, BG_SURFACE, BORDER_STRONG, BORDER_SUBTLE, FONT_MONO,
        TEXT_MUTED, TEXT_PRIMARY, TEXT_SECONDARY, TRACK_BG, provider_display, provider_short,
        with_alpha,
    },
};

pub fn render_provider_share(
    shares: &[ProviderShare],
    assets: &Assets,
    selected_provider: Option<&str>,
    cx: &mut Context<Dashboard>,
) -> impl IntoElement + use<> {
    let header_note = selected_provider
        .map(|s| format!("Filtered: {}", provider_display(s)))
        .unwrap_or_else(|| format!("{} active", shares.len()));

    let mut bar_segs = Vec::with_capacity(shares.len());
    let mut chips = Vec::with_capacity(shares.len());
    for (idx, p) in shares.iter().enumerate() {
        let is_selected = selected_provider == Some(p.slug.as_str());
        let dimmed = selected_provider.is_some() && !is_selected;
        let col = if dimmed {
            rgba(with_alpha(p.color, 0x45))
        } else {
            rgb(p.color)
        };
        let target_bar = (!is_selected).then(|| p.slug.clone());
        bar_segs.push(
            div()
                .id(("prov-bar", idx))
                .h_full()
                .w(relative(p.share.max(0.01)))
                .bg(col)
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.act(Action::ProviderFilter(target_bar.clone()), cx);
                })),
        );

        let pct = (p.share * 100.0).round() as u32;
        let target_chip = (!is_selected).then(|| p.slug.clone());
        let icon = assets.provider_icon(&p.slug);
        let short = provider_short(&p.slug);
        let color = p.color;
        chips.push(
            div()
                .id(("prov-chip", idx))
                .px(px(7.))
                .py(px(3.5))
                .rounded(px(6.5))
                .cursor_pointer()
                .border_1()
                .border_color(rgba(if is_selected {
                    BORDER_STRONG
                } else {
                    BORDER_SUBTLE
                }))
                .bg(if is_selected {
                    rgba((BG_ELEVATED << 8) | 0xFF)
                } else {
                    rgba(0x00000000)
                })
                .hover(move |style| {
                    if is_selected {
                        style
                    } else {
                        style.bg(rgba(BG_HOVER))
                    }
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.act(Action::ProviderFilter(target_chip.clone()), cx);
                }))
                .flex()
                .items_center()
                .gap(px(5.))
                .child(if let Some(icon) = icon {
                    img(icon)
                        .size(px(13.))
                        .rounded(px(3.))
                        .object_fit(ObjectFit::Contain)
                        .into_any_element()
                } else {
                    div()
                        .size(px(13.))
                        .rounded(px(3.))
                        .bg(rgb(BG_ELEVATED))
                        .flex()
                        .items_center()
                        .justify_center()
                        .font_family(FONT_MONO)
                        .text_size(px(8.5))
                        .text_color(rgb(color))
                        .child(short)
                        .into_any_element()
                })
                .child(
                    div()
                        .text_size(px(11.))
                        .when(is_selected, |d| d.font_weight(FontWeight::MEDIUM))
                        .text_color(rgb(if is_selected {
                            TEXT_PRIMARY
                        } else {
                            TEXT_SECONDARY
                        }))
                        .child(p.display.clone()),
                )
                .child(
                    div()
                        .font_family(FONT_MONO)
                        .text_size(px(10.5))
                        .text_color(rgb(TEXT_MUTED))
                        .child(format!("{pct}%")),
                ),
        );
    }

    div()
        .bg(rgb(BG_SURFACE))
        .border_1()
        .border_color(rgba(BORDER_SUBTLE))
        .rounded(px(10.))
        .p(px(12.))
        .flex()
        .flex_col()
        .gap(px(9.))
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap(px(8.))
                .child(
                    div()
                        .text_size(px(10.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(TEXT_SECONDARY))
                        .child("PROVIDERS"),
                )
                .child(
                    div()
                        .font_family(FONT_MONO)
                        .text_size(px(10.5))
                        .text_color(rgb(TEXT_MUTED))
                        .child(header_note),
                ),
        )
        .child(
            div()
                .h(px(6.5))
                .w_full()
                .rounded_full()
                .bg(rgba(TRACK_BG))
                .overflow_hidden()
                .flex()
                .gap(px(1.5))
                .children(bar_segs),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(5.))
                .child(chip_button(
                    "provider-all",
                    "All",
                    selected_provider.is_none(),
                    Action::ProviderFilter(None),
                    cx,
                ))
                .children(chips),
        )
}
