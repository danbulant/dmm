use dmm::games::GameKind;
use dmm::sources::{ModFile, ModImage};
use rune::tok::{Theme, type_};
use rune::{El, Icon, ScrollSpec, SurfaceFit, col, el, row, text};
use rune_kit::{Checkbox, Dialog, ImageZoom, Kit, Preview};

use super::{Detail, Screen, State};
use crate::desktop::Size;
use crate::desktop::components::{
    button, form_field, muted, open_page, page_url, panel, source_icon, source_name,
};
use crate::desktop::mod_card::bookmark;

static ZOOM_PLUS: Icon = Icon {
    name: "dmm-zoom-plus",
    view: 16.0,
    body: r#"<path d="M8 4V12M4 8H12" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>"#,
};

pub(in crate::desktop) fn header(s: &State, detail: &Detail, t: Theme, k: &Kit) -> El {
    let info = &detail.info;
    let mut meta = row()
        .gap(6.0)
        .items_center()
        .flex_wrap()
        .child(source_icon(s, info.id.source, 14.0))
        .child(muted(source_name(info.id.source), t));
    for label in [
        info.author.as_deref(),
        info.category.as_deref(),
        info.version.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        meta = meta.child(muted(format!("·  {label}"), t).truncate());
    }
    let available = s.matches_instance(&info.id);
    let has_files = !info.files.is_empty();
    let mut header = row()
        .key("detail-header")
        .w_full()
        .items_center()
        .gap(12.0)
        .padding(16.0)
        .fill(k.c.layer_01)
        .border(1.0, t.separator)
        .child(
            button("details-back", "←", t, false, true, |s, tx| {
                s.set_screen(s.screen, tx)
            })
            .label("Back to mods")
            .help("Back to mods"),
        )
        .child(
            col()
                .gap(5.0)
                .grow(1.0)
                .shrink()
                .min_w(0.0)
                .child(
                    text(&info.name)
                        .font(type_::TITLE)
                        .color(t.text)
                        .truncate()
                        .help(&info.name),
                )
                .child(meta),
        );
    let thumbnail = info
        .images
        .first()
        .map(|image| image.thumbnail.as_deref().unwrap_or(&image.url));
    header = header.child(bookmark(
        s,
        "details-save",
        &info.id,
        &info.name,
        thumbnail,
        t,
        k,
    ));
    if s.local_detail.is_some() {
        return header;
    }
    let queued = s.queued_for_selected(&info.id);
    header = header.child(
        button(
            "details-install",
            if queued {
                "In Downloads"
            } else if s.installed_for(&info.id).is_some() {
                "Reinstall"
            } else {
                "Install"
            },
            t,
            true,
            queued || (has_files && available && s.queue.available()),
            move |s, tx| {
                if queued {
                    s.open_downloads(tx);
                } else if let Some(detail) = &mut s.browser.detail {
                    detail.install_open = true;
                }
            },
        )
        .help(if queued {
            "View this mod in the download queue"
        } else if !has_files {
            "No downloadable files from this source"
        } else if !available {
            "Select a game installation before installing"
        } else {
            "Choose files and install options"
        }),
    );
    header
}

pub(in crate::desktop) fn body(s: &State, detail: &Detail, t: Theme) -> El {
    let info = &detail.info;
    let mut view = col().w_full().gap(18.0);
    if !info.images.is_empty() {
        view = view.child(gallery(s, detail, t));
    }
    if !info.requirements.is_empty() {
        let mut requirements = section("Requirements", t);
        for (index, dep) in info.requirements.iter().enumerate() {
            let mut row = row()
                .min_h(36.0)
                .w_full()
                .items_center()
                .gap(10.0)
                .px(10.0)
                .py(6.0);
            if let (Some(previews), Some(icon)) = (&s.previews, dep.thumbnail.as_deref()) {
                row = row.child(previews.icon(Some(icon), t));
            }
            let mut label = col()
                .min_w(0.0)
                .grow(1.0)
                .gap(3.0)
                .child(text(&dep.name).font(type_::LABEL).color(t.text).wrap());
            if let Some(notes) = &dep.notes {
                label = label.child(muted(notes, t).wrap());
            }
            row = row.child(label);
            if let Some(raw_url) = dep.url.as_deref()
                && let Some(url) = page_url(raw_url)
            {
                row = row.child(
                    button(
                        format!("requirement-{index}"),
                        "Open ↗",
                        t,
                        false,
                        true,
                        move |s, tx| open_page(s, tx, url.as_str()),
                    )
                    .label(format!("Open {} requirement", dep.name))
                    .help(format!("Open requirement: {}", dep.name)),
                );
            }
            requirements = requirements.child(row);
        }
        view = view.child(requirements);
    }
    let mut actions = row().gap(8.0).flex_wrap();
    if let Some(url) = info.url.clone() {
        actions = actions.child(button(
            "details-source",
            "Open source page ↗",
            t,
            false,
            true,
            move |s, tx| open_page(s, tx, &url),
        ));
    }
    let about = section("About", t)
        .child(
            text(if detail.description.is_empty() {
                "The source did not provide a description."
            } else {
                &detail.description
            })
            .font(type_::BODY.lh(20.0))
            .color(t.text)
            .wrap(),
        )
        .child(actions);
    view.child(about).child(files(detail, t, None))
}

fn section(title: &str, t: Theme) -> El {
    col()
        .w_full()
        .gap(10.0)
        .child(text(title).font(type_::TITLE).color(t.text))
}

pub(in crate::desktop) fn close_install(s: &mut State) {
    if let Some(detail) = &mut s.browser.detail {
        detail.install_open = false;
    }
}

pub(in crate::desktop) fn open_install(s: &mut State) {
    if let Some(detail) = &mut s.browser.detail {
        detail.install_open = true;
    }
}

fn close_image(s: &mut State) {
    if let Some(detail) = &mut s.browser.detail {
        detail.full_image_open = false;
    }
}

fn image_zoom(s: &mut State) -> &mut ImageZoom {
    &mut s
        .browser
        .detail
        .as_mut()
        .expect("image detail is open")
        .zoom
}

pub(in crate::desktop) fn overlays(
    s: &State,
    detail: &Detail,
    t: Theme,
    k: &Kit,
    layer: &mut Vec<El>,
    reduced_motion: bool,
) {
    if detail.install_open {
        let content = if s.matches_instance(&detail.info.id) {
            install_options(s, detail, t, k)
        } else {
            col()
                .w_full()
                .gap(12.0)
                .child(muted("Choose a main installation for this game in Game setup.", t).wrap())
                .child(button(
                    "needs-setup",
                    "Open Game setup",
                    t,
                    true,
                    true,
                    |s, tx| s.set_screen(Screen::Setup, tx),
                ))
        };
        let title = if s.installed_for(&detail.info.id).is_some() {
            "Reinstall mod"
        } else {
            "Install mod"
        };
        Dialog::new("mod-install", title)
            .description(&detail.info.name)
            .open(true)
            .fit()
            .width(760.0)
            .on_close(close_install)
            .child(col().w_full().px(16.0).pb(16.0).child(content))
            .build(k, layer);
    }
    if detail.full_image_open
        && let Some(image) = detail.info.images.get(detail.image)
    {
        let decoded = s.previews.as_ref().and_then(|previews| {
            previews
                .decoded(Size::Gallery, Some(&image.url))
                .or_else(|| previews.decoded(Size::Thumbnail, Some(thumbnail(image))))
        });
        let header = row()
            .w_full()
            .items_center()
            .gap(12.0)
            .child(muted(&detail.info.name, t).grow(1.0).shrink().truncate())
            .child(image_navigation(detail, t, "expanded-image"));
        let mut overlay = col()
            .key("full-image")
            .role("dialog")
            .label("Expanded mod screenshot")
            .pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0))
            .fill(t.bg);
        if let Some((pixels, natural)) = decoded {
            let preview = Preview::new(
                image_zoom,
                natural,
                el().key("expanded-gallery-image")
                    .pixels_fit(pixels, SurfaceFit::Fit),
            )
            .icons(k.glyph.dash, &ZOOM_PLUS)
            .labels("Zoom out", "Fit screenshot", "Zoom in", "Close screenshot")
            .on_close(|s: &mut State, _| close_image(s))
            .build(&detail.zoom, k, s.window_size, reduced_motion)
            .child(
                row()
                    .pin(Some(16.0), Some(10.0), Some(16.0), None)
                    .child(header),
            );
            overlay = overlay.child(preview);
        } else {
            overlay = overlay
                .padding(16.0)
                .gap(8.0)
                .modal(false, close_image)
                .child(
                    row()
                        .gap(8.0)
                        .items_center()
                        .child(header.grow(1.0).shrink())
                        .child(button(
                            "close-full-image",
                            "Close ×",
                            t,
                            true,
                            true,
                            |s, _| close_image(s),
                        )),
                )
                .child(image_surface(
                    s,
                    image,
                    el().w_full().grow(1.0).shrink().min_h(0.0),
                    t,
                ));
        }
        layer.push(overlay);
    }
}

fn gallery(s: &State, detail: &Detail, t: Theme) -> El {
    let images = &detail.info.images;
    let Some(image) = images.get(detail.image) else {
        return col();
    };
    let hero = image_surface(s, image, el().w_full().h(350.0), t)
        .key("gallery-main")
        .role("button")
        .label("Expand screenshot")
        .help("Expand screenshot")
        .focusable(true)
        .on_press(|s: &mut State| {
            if let Some(detail) = &mut s.browser.detail {
                detail.zoom.reset();
                detail.full_image_open = true;
            }
        });
    let mut gallery = col().w_full().gap(9.0).child(hero);
    if images.len() > 1 {
        let strip = row()
            .gap(8.0)
            .children(images.iter().enumerate().map(|(index, image)| {
                let frame = el().size(88.0, 58.0).clip_rounded(5.0).border(
                    2.0,
                    if detail.image == index {
                        t.text_secondary
                    } else {
                        t.separator
                    },
                );
                render(
                    s,
                    Some(thumbnail(image)),
                    Size::Thumbnail,
                    frame,
                    SurfaceFit::Fill,
                    t,
                )
                .key(format!("gallery-thumb-{index}"))
                .role("button")
                .label(format!("Screenshot {}", index + 1))
                .focusable(true)
                .focus_selected(index == detail.image)
                .on_press(move |s: &mut State| s.gallery_step(index))
            }));
        gallery = gallery.child(row().w_full().h(68.0).scroll_x().child(strip));
    }
    gallery.child(image_navigation(detail, t, "image"))
}

fn image_navigation(detail: &Detail, t: Theme, key: &str) -> El {
    let index = detail.image;
    let count = detail.info.images.len();
    row()
        .items_center()
        .justify_between()
        .gap(8.0)
        .child(muted(format!("Screenshot {} of {count}", index + 1), t))
        .child(
            row()
                .gap(8.0)
                .child(button(
                    format!("{key}-prev"),
                    "Previous",
                    t,
                    false,
                    index > 0,
                    move |s, _| s.gallery_step(index.saturating_sub(1)),
                ))
                .child(button(
                    format!("{key}-next"),
                    "Next",
                    t,
                    false,
                    index + 1 < count,
                    move |s, _| s.gallery_step(index + 1),
                )),
        )
}

fn thumbnail(image: &ModImage) -> &str {
    image.thumbnail.as_deref().unwrap_or(&image.url)
}

pub(in crate::desktop) fn preview_requests(
    detail: &Detail,
    saved: bool,
) -> impl Iterator<Item = (&str, Size)> {
    let image = detail.info.images.get(detail.image);
    let gallery = image.map(|image| (image.url.as_str(), Size::Gallery));
    let thumbnails = detail
        .info
        .images
        .iter()
        .map(|image| (thumbnail(image), Size::Thumbnail));
    gallery
        .filter(|_| !saved)
        .into_iter()
        .chain(thumbnails)
        .chain(gallery.filter(|_| saved))
        .chain(
            detail
                .info
                .requirements
                .iter()
                .filter_map(|dep| dep.thumbnail.as_deref())
                .map(|url| (url, Size::Icon)),
        )
}

fn image_surface(s: &State, image: &ModImage, frame: El, t: Theme) -> El {
    render(
        s,
        Some(&image.url),
        Size::Gallery,
        frame.clip_rounded(8.0),
        SurfaceFit::Fit,
        t,
    )
}

fn render(s: &State, url: Option<&str>, size: Size, frame: El, fit: SurfaceFit, t: Theme) -> El {
    match &s.previews {
        Some(previews) => previews.surface(url, size, frame, fit, t),
        None if size == Size::Thumbnail => frame.fill(t.control_off),
        None => frame
            .fill(t.control_off)
            .center()
            .child(muted("Preview unavailable", t)),
    }
}

fn files(detail: &Detail, t: Theme, picker: Option<&Kit>) -> El {
    let mut view = section(
        if picker.is_some() {
            "Choose files"
        } else {
            "Files"
        },
        t,
    );
    if detail.info.files.is_empty() {
        return view.child(muted("The source did not list downloadable files.", t));
    }
    for file in &detail.info.files {
        let id = file.id;
        let chosen = detail.selected_files.contains(&id);
        let title = if let Some(k) = picker {
            Checkbox::new(chosen)
                .label(&file.name)
                .on_toggle(move |s: &mut State| {
                    if let Some(detail) = &mut s.browser.detail
                        && !detail.selected_files.remove(&id)
                    {
                        detail.selected_files.insert(id);
                    }
                })
                .build(k)
                .key(format!("file-{id}"))
        } else {
            text(&file.name).font(type_::LABEL).color(t.text).wrap()
        };
        let mut entry = col()
            .w_full()
            .gap(5.0)
            .padding(10.0)
            .radius(8.0)
            .fill(if picker.is_some() && chosen {
                t.control_track
            } else {
                t.card
            })
            .border(1.0, t.separator)
            .child(title)
            .child(muted(file_metadata(file), t).wrap());
        if let Some(description) = &file.description
            && !description.trim().is_empty()
        {
            entry = entry.child(muted(description, t).wrap());
        }
        view = view.child(entry);
    }
    view
}

fn file_metadata(file: &ModFile) -> String {
    let mut items = Vec::new();
    if file.primary {
        items.push("Main file".to_owned());
    }
    if let Some(category) = &file.category
        && (!file.primary || !category.eq_ignore_ascii_case("main"))
    {
        items.push(category.clone());
    }
    if let Some(version) = &file.version {
        items.push(format!("Version {version}"));
    }
    if let Some(date) = &file.updated_at {
        items.push(format!(
            "Uploaded {}",
            date.split('T').next().unwrap_or(date)
        ));
    }
    if let Some(size) = file.size {
        const MIB: f64 = 1024.0 * 1024.0;
        items.push(if size < 1024 {
            format!("{size} B")
        } else if size < MIB as u64 {
            format!("{:.1} KiB", size as f64 / 1024.0)
        } else {
            format!("{:.1} MiB", size as f64 / MIB)
        });
    }
    if let Some(downloads) = file.downloads {
        items.push(format!("{downloads} downloads"));
    }
    items.join("  ·  ")
}

fn install_options(s: &State, detail: &Detail, t: Theme, k: &Kit) -> El {
    let enabled = s.queue.available();
    let mut content = panel(t)
        .children(detail.missing_previous_files.then(|| {
            muted(
                "Some installed files are no longer listed. Choose the complete replacement.",
                t,
            )
            .wrap()
        }))
        .child(
            col()
                .w_full()
                .max_h((s.window_size[1] - 290.0).clamp(145.0, 440.0))
                .scroll(ScrollSpec {
                    stick_bottom: false,
                })
                .child(files(detail, t, Some(k))),
        );
    if s.game_kind() != GameKind::Zzmi {
        content = content.child(
            col()
                .w_full()
                .gap(5.0)
                .child(muted("Destination override (optional)", t))
                .child(form_field(
                    "remote-into",
                    "Default: detected game folder layout",
                    &s.browser.install_into,
                    k,
                    t,
                    |s| &mut s.browser.install_into,
                )),
        );
    }
    let controls = row()
        .w_full()
        .items_center()
        .justify_between()
        .gap(8.0)
        .children(s.installed_for(&detail.info.id).is_none().then(|| {
            Checkbox::new(s.preferences.enable_new_mods)
                .label("Enable after installing")
                .disabled(!enabled)
                .on_toggle(|s: &mut State| s.set_enable_new_mods(!s.preferences.enable_new_mods))
                .build(k)
                .key("remote-stage")
        }))
        .child(button(
            "remote-install",
            format!("Queue install ({})", detail.selected_files.len()),
            t,
            true,
            enabled && !detail.selected_files.is_empty(),
            |s, _| s.install_selected(),
        ));
    content
        .child(controls)
        .children((!s.notice.is_empty()).then(|| muted(&s.notice, t).wrap()))
}
