use dmm::library::ManagedMod;
use dmm::sources::RemoteMod;
use rune::tok::Theme;
use rune::{El, Icon, SurfaceFit, col, el, icon, row};
use rune_kit::{Kit, Switch};

use super::components::{button, card_title, mod_card, muted, preview};
use super::State;

static BOOKMARK: Icon = Icon {
    name: "dmm-bookmark",
    view: 16.0,
    body: r#"<path d="M4 2.5h8v11l-4-2.8-4 2.8z" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linejoin="round"/>"#,
};
static BOOKMARK_SAVED: Icon = Icon {
    name: "dmm-bookmark-saved",
    view: 16.0,
    body: r#"<path d="M4 2.5h8v11l-4-2.8-4 2.8z" fill="currentColor" stroke="currentColor" stroke-width="1.4" stroke-linejoin="round"/>"#,
};

pub(super) enum CardSource<'a> {
    Remote(&'a RemoteMod),
    Installed {
        instance: &'a str,
        record: &'a ManagedMod,
    },
}

pub(super) struct Card<'a> {
    pub key: String,
    pub name: &'a str,
    pub image: Option<&'a str>,
    pub byline: String,
    pub metadata: String,
    pub source: CardSource<'a>,
}

impl Card<'_> {
    pub(super) fn view(self, s: &State, t: Theme, k: &Kit) -> El {
        let (remote, installed, details_key) = match self.source {
            CardSource::Remote(id) => (
                Some(id),
                s.snapshot
                    .selected
                    .as_ref()
                    .zip(s.snapshot.installed_remote(id))
                    .map(|(instance, record)| (instance.name.as_str(), record)),
                format!("{}:details", self.key),
            ),
            CardSource::Installed { instance, record } => (
                record.release().map(|release| &release.id),
                Some((instance, record)),
                format!("mod:{instance}:{}:details", record.slug),
            ),
        };
        let mut details = col()
            .key(details_key)
            .w_full()
            .gap(5.0)
            .radius(9.0)
            .role("button")
            .label(format!("Details for {}", self.name))
            .focus_ring(2.0, 0.0, k.c.border_focus)
            .child(preview(s, self.image, 128.0, SurfaceFit::Fill, t))
            .child(card_title(self.name, t))
            .child(muted(self.byline, t).h(17.0).w_full().truncate())
            .child(muted(self.metadata, t).h(17.0).w_full().truncate());
        if let CardSource::Installed { record, .. } = self.source {
            let slug = record.slug.clone();
            details = details
                .focusable(true)
                .hover(t.hover)
                .on_press_tx(move |s: &mut State, tx| s.open_installed_details(&slug, tx));
        } else if let Some(id) = remote {
            let id = id.clone();
            let enabled = s.details_busy.is_none();
            details = details.focusable(enabled).disabled(!enabled);
            if enabled {
                details = details
                    .hover(t.hover)
                    .on_press(move |s: &mut State| s.details(id.clone()));
            }
        }
        let footer = if let Some(status) = remote.and_then(|id| s.pending_install(id)) {
            button(
                format!("{}:download", self.key),
                status,
                t,
                false,
                true,
                |s, tx| s.open_downloads(tx),
            )
            .label(format!("{}: {status} View Downloads", self.name))
            .help("View in Downloads")
        } else if let Some((instance, record)) = installed {
            installed_toggle(s, instance, record, self.name, t, k)
        } else if let Some(id) = remote {
            let can_install = s.install_blocker(id).is_none();
            let id = id.clone();
            button(
                format!("{}:install", self.key),
                "Install",
                t,
                true,
                can_install,
                move |s, tx| s.request_install(&id, tx),
            )
        } else {
            el().h(32.0)
        };
        mod_card(t)
            .key(&self.key)
            .child(details)
            .children(remote.map(|id| {
                bookmark(
                    s,
                    format!("{}:bookmark", self.key),
                    id,
                    self.name,
                    self.image,
                    t,
                    k,
                )
                .pin(Some(15.0), Some(15.0), None, None)
            }))
            .child(footer.w_full())
    }
}

pub(super) fn bookmark(
    s: &State,
    key: impl Into<String>,
    id: &RemoteMod,
    name: &str,
    image: Option<&str>,
    t: Theme,
    k: &Kit,
) -> El {
    let saved = s.snapshot.is_saved(id);
    let enabled = s.busy.is_none();
    let label = if saved {
        "Remove bookmark"
    } else {
        "Save bookmark"
    };
    let mut control = el()
        .key(key.into())
        .size(32.0, 32.0)
        .center()
        .radius(8.0)
        .role("checkbox")
        .label(format!("Bookmark {name}"))
        .checked(saved)
        .help(label)
        .fill(k.c.layer_01)
        .border(1.0, k.c.border_base)
        .focus_ring(2.0, 0.0, k.c.border_focus)
        .focusable(enabled)
        .disabled(!enabled)
        .child(
            icon(if saved { &BOOKMARK_SAVED } else { &BOOKMARK }, 17.0)
                .color(if saved { t.text } else { t.text_secondary })
                .a11y_hidden(),
        );
    if enabled {
        let id = id.clone();
        let name = name.to_owned();
        let image = image.map(str::to_owned);
        control = control.hover(t.hover).on_press(move |s: &mut State| {
            s.toggle_bookmark(id.clone(), name.clone(), image.clone());
        });
    }
    control
}

fn installed_toggle(
    s: &State,
    instance: &str,
    record: &ManagedMod,
    name: &str,
    t: Theme,
    k: &Kit,
) -> El {
    let key = format!("mod:{instance}:{}:enabled", record.slug);
    let game = instance.to_owned();
    let slug = record.slug.clone();
    let enabled = record.enabled;
    row()
        .w_full()
        .min_h(32.0)
        .items_center()
        .justify_between()
        .child(muted(if enabled { "Enabled" } else { "Disabled" }, t))
        .child(
            enable_switch(
                s,
                instance,
                name,
                enabled,
                move |state| {
                    if state
                        .snapshot
                        .selected
                        .as_ref()
                        .is_some_and(|selected| selected.name == game)
                    {
                        state.set_mod_enabled(game.clone(), slug.clone(), !enabled, None);
                    }
                },
                k,
            )
            .key(key),
        )
}

pub(super) fn enable_switch(
    s: &State,
    instance: &str,
    name: &str,
    enabled: bool,
    on_toggle: impl Fn(&mut State) + 'static,
    k: &Kit,
) -> El {
    Switch::new(enabled)
        .label(format!("Enable {name}"))
        .hide_label()
        .disabled(!s.can_toggle_mod(instance))
        .on_toggle(on_toggle)
        .build(k)
}

pub(super) fn remote_key(id: &RemoteMod) -> String {
    format!("{}-{}-{}", id.source, id.game, id.id)
}

pub(super) fn byline(author: Option<&str>, category: Option<&str>, fallback: &str) -> String {
    match (author, category) {
        (Some(author), Some(category)) => format!("{author} · {category}"),
        (Some(label), None) | (None, Some(label)) => label.to_owned(),
        (None, None) => fallback.to_owned(),
    }
}
