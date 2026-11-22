use dmm::sources::Source;
use rune::tok::{Theme, type_};
use rune::{El, Pixels, SurfaceFit, TextEdit, TextStyle, Tx, col, el, oklch, row, text};
use rune_kit::{Kit, TextInput};
use url::Url;

use super::State;

pub(super) fn kit(dark: bool) -> Kit {
    let mut kit = Kit::neutral(dark);
    kit.c.text_faint = kit.c.text_muted;
    let border = control_border(dark);
    kit.c.border_muted = border;
    kit.c.border_base = border;
    kit.c.std_border = border;
    kit.c.layer_01 = control_background(dark);
    kit.c.icon_faint = kit.c.icon_muted;
    kit
}

fn control_border(dark: bool) -> rune::Color {
    if dark {
        oklch(0.54, 0.022, 272.0)
    } else {
        oklch(0.55, 0.022, 272.0)
    }
}

fn control_background(dark: bool) -> rune::Color {
    if dark {
        oklch(0.255, 0.018, 272.0)
    } else {
        oklch(0.985, 0.008, 272.0)
    }
}

pub(super) fn button(
    key: impl Into<String>,
    label: impl Into<String>,
    t: Theme,
    active: bool,
    enabled: bool,
    action: impl Fn(&mut State, &mut Tx) + 'static,
) -> El {
    let label = label.into();
    let on = active && enabled;
    let mut item = row()
        .key(key.into())
        .role("button")
        .label(&label)
        .focusable(enabled)
        .disabled(!enabled)
        .min_h(32.0)
        .px(10.0)
        .center()
        .radius(8.0)
        .fill(if on { oklch(0.49, 0.16, 282.0) } else { t.card })
        .border(1.0, t.card_border)
        .child(
            text(label)
                .font(type_::LABEL)
                .color(if on {
                    oklch(0.99, 0.0, 282.0)
                } else if enabled {
                    t.text
                } else {
                    t.text_secondary
                })
                .truncate(),
        );
    if enabled {
        item = item.hover(t.hover).on_press_tx(action);
    }
    item
}

pub(super) fn field(
    edit: &TextEdit,
    placeholder: &str,
    accessor: impl for<'a> Fn(&'a mut State) -> &'a mut TextEdit + 'static,
) -> TextInput {
    TextInput::new(edit)
        .placeholder(placeholder)
        .width(None)
        .height(34.0)
        .on_edit(accessor)
}

pub(super) fn styled_field(key: &str, edit: &TextEdit, input: TextInput, k: &Kit, t: Theme) -> El {
    input.build(k).key(key).fill(t.card).border(
        1.0,
        if edit.focused {
            k.c.border_focus
        } else {
            k.c.border_base
        },
    )
}

pub(super) fn form_field(
    key: &str,
    placeholder: &str,
    edit: &TextEdit,
    k: &Kit,
    t: Theme,
    accessor: impl for<'a> Fn(&'a mut State) -> &'a mut TextEdit + 'static,
) -> El {
    styled_field(key, edit, field(edit, placeholder, accessor), k, t)
}

pub(super) fn heading(label: impl Into<String>, t: Theme) -> El {
    text(label.into())
        .font(TextStyle::new(20.0, 670.0))
        .color(t.text)
        .wrap()
}

pub(super) fn muted(label: impl Into<String>, t: Theme) -> El {
    text(label.into())
        .font(type_::LABEL)
        .color(t.text_secondary)
}

pub(super) fn panel(t: Theme) -> El {
    col()
        .w_full()
        .gap(10.0)
        .padding(14.0)
        .radius(11.0)
        .fill(t.card)
        .border(1.0, t.card_border)
}

pub(super) fn mod_card(t: Theme) -> El {
    col()
        .w(248.0)
        .padding(9.0)
        .gap(7.0)
        .radius(12.0)
        .fill(t.card)
        .border(1.0, t.card_border)
}

pub(super) fn card_title(name: &str, t: Theme) -> El {
    text(name)
        .font(TextStyle::new(14.0, 680.0))
        .color(t.text)
        .w_full()
        .truncate()
        .help(name)
}

pub(super) fn icon(bytes: &[u8]) -> Option<Pixels> {
    let rgba = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
        .ok()?
        .into_rgba8();
    let pixels = Pixels::new();
    pixels.set(rgba.width(), rgba.height(), rgba.into_raw());
    Some(pixels)
}

pub(super) fn source_icon(s: &State, source: Source, side: f32) -> El {
    let pixels = match source {
        Source::Nexus => s.nexus_icon.as_ref(),
        Source::GameBanana => s.gamebanana_icon.as_ref(),
        Source::Local => None,
    };
    let frame = el().size(side, side).a11y_hidden();
    match pixels {
        Some(pixels) => frame.pixels_fit(pixels, SurfaceFit::Fit),
        None => frame,
    }
}

pub(super) fn preview(s: &State, url: Option<&str>, height: f32, fit: SurfaceFit, t: Theme) -> El {
    if let Some(previews) = &s.previews {
        previews.view(url, height, fit, t)
    } else {
        el().w_full()
            .h(height)
            .fill(t.control_off)
            .center()
            .child(muted("Preview unavailable", t))
    }
}

pub(super) fn page_url(raw: &str) -> Option<Url> {
    let url = Url::parse(raw.trim()).ok()?;
    (matches!(url.scheme(), "http" | "https")
        && url.has_host()
        && url.username().is_empty()
        && url.password().is_none())
    .then_some(url)
}

pub(super) fn open_page(s: &mut State, tx: &mut Tx, raw: &str) {
    let Some(url) = page_url(raw) else {
        s.notice = "Source link is not a valid HTTP or HTTPS URL.".into();
        return;
    };
    if let Err(error) = tx.open_url(url.as_str()) {
        s.notice = error.to_string();
    }
}

pub(super) fn source_name(source: Source) -> &'static str {
    match source {
        Source::Nexus => "Nexus Mods",
        Source::GameBanana => "GameBanana",
        Source::Local => "Local file",
    }
}

pub(super) fn bytes(value: u64) -> String {
    if value < 1024 {
        return format!("{value} B");
    }
    let mut amount = value as f64;
    for unit in ["KiB", "MiB", "GiB", "TiB", "PiB", "EiB"] {
        amount /= 1024.0;
        if amount < 1024.0 {
            return format!("{amount:.1} {unit}");
        }
    }
    unreachable!("u64 byte counts fit in EiB")
}
