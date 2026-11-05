//! The "Sources" group of the preferences window: one row per package source, switches, add,
//! remove and refresh. Plain gtk; `on_change` asks the window to reload the package list.
//! Names, refs and errors come from files and flakes, so every label here has markup off.

use super::sourceinfo;
use adw::prelude::*;
use nsc_backend::sources::{self, Source};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

pub struct SourcesGroup {
    pub group: adw::PreferencesGroup,
    rows: RefCell<Vec<adw::ActionRow>>,
    refresh: gtk::Button,
    busy: Cell<bool>,
    on_change: Box<dyn Fn()>,
}

/// An error on one line, cut to a readable length.
fn short(e: &str) -> String {
    let joined = e.lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" | ");
    joined.chars().take(240).collect()
}

fn time_text(t: std::time::SystemTime) -> String {
    let secs = t.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    gtk::glib::DateTime::from_unix_local(secs)
        .ok()
        .and_then(|d| d.format("%Y-%m-%d %H:%M").ok())
        .map_or_else(|| "unknown".to_string(), |s| s.to_string())
}

/// Subtitle second line and whether it is an error.
pub fn status_text(src: &Source, rejected: &[(String, String)]) -> (String, bool) {
    if src.builtin {
        return ("Built in".into(), false);
    }
    if let Some((_, why)) = rejected.iter().find(|r| r.0 == src.id) {
        return (format!("Rejected: {}", short(why)), true);
    }
    match sources::status(&src.id) {
        Err(e) => (format!("Failed: {}", short(&e)), true),
        Ok(Some(t)) => (format!("Updated {}", time_text(t)), false),
        Ok(None) => ("Not fetched yet".into(), false),
    }
}

impl std::fmt::Debug for SourcesGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SourcesGroup")
    }
}

impl SourcesGroup {
    pub fn new(on_change: impl Fn() + 'static) -> Rc<Self> {
        let group = adw::PreferencesGroup::new();
        group.set_title("Sources");
        group.set_description(Some("Where apps come from. Hiding a source does not change apps you installed."));
        let refresh = gtk::Button::with_label("Refresh");
        refresh.set_valign(gtk::Align::Center);
        let add = gtk::Button::with_label("Add Source…");
        add.set_valign(gtk::Align::Center);
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        buttons.append(&refresh);
        buttons.append(&add);
        group.set_header_suffix(Some(&buttons));
        let this = Rc::new(SourcesGroup {
            group,
            rows: RefCell::new(vec![]),
            refresh: refresh.clone(),
            busy: Cell::new(false),
            on_change: Box::new(on_change),
        });
        {
            let this = this.clone();
            refresh.connect_clicked(move |_| this.refresh_all());
        }
        {
            let this = this.clone();
            add.connect_clicked(move |_| this.add_dialog("", "", None));
        }
        this.rebuild();
        this
    }

    fn window(&self) -> Option<gtk::Window> {
        self.group.root().and_then(|r| r.downcast::<gtk::Window>().ok())
    }

    fn toast(&self, text: &str) {
        if let Some(w) = self.window().and_then(|w| w.downcast::<adw::PreferencesWindow>().ok()) {
            // Toast titles are markup.
            w.add_toast(adw::Toast::new(&sourceinfo::escape(text)));
        }
    }

    pub fn rebuild(self: &Rc<Self>) {
        for r in self.rows.borrow_mut().drain(..) {
            self.group.remove(&r);
        }
        let store = sourceinfo::store();
        let (_, rejected) = sourceinfo::last_report();
        for src in store.sources() {
            let row = self.row(&src, &rejected);
            self.group.add(&row);
            self.rows.borrow_mut().push(row);
        }
    }

    fn row(self: &Rc<Self>, src: &Source, rejected: &[(String, String)]) -> adw::ActionRow {
        let row = adw::ActionRow::new();
        // Titles and subtitles are shown as they are.
        row.set_use_markup(false);
        row.set_title(&src.name);
        let (status, failed) = status_text(src, rejected);
        row.set_subtitle(&format!("{}\n{}", src.flake, status));
        row.set_subtitle_lines(0);
        if failed {
            row.add_prefix(&gtk::Image::from_icon_name("dialog-warning-symbolic"));
        }
        if !src.builtin {
            let remove = gtk::Button::from_icon_name("user-trash-symbolic");
            remove.set_valign(gtk::Align::Center);
            remove.add_css_class("flat");
            remove.set_tooltip_text(Some("Remove source"));
            let this = self.clone();
            let src = src.clone();
            remove.connect_clicked(move |_| this.remove_dialog(&src));
            row.add_suffix(&remove);
        }
        let switch = gtk::Switch::new();
        switch.set_valign(gtk::Align::Center);
        switch.set_active(src.enabled);
        let this = self.clone();
        let id = src.id.clone();
        switch.connect_state_set(move |_, on| {
            let mut store = sourceinfo::store();
            store.set_enabled(&id, on);
            match store.save() {
                Ok(()) => (this.on_change)(),
                Err(e) => this.toast(&format!("Could not save the sources: {e}")),
            }
            gtk::Inhibit(false)
        });
        row.add_suffix(&switch);
        row.set_activatable_widget(Some(&switch));
        row
    }

    fn confirm(&self, heading: &str, body: &str, ok: &str, f: impl Fn() + 'static) {
        let d = adw::MessageDialog::new(self.window().as_ref(), Some(heading), Some(body));
        d.set_heading_use_markup(false);
        d.set_body_use_markup(false);
        d.add_response("cancel", "Cancel");
        d.add_response("ok", ok);
        d.set_response_appearance("ok", adw::ResponseAppearance::Destructive);
        d.set_default_response(Some("cancel"));
        d.set_close_response("cancel");
        d.connect_response(None, move |_, r| {
            if r == "ok" {
                f();
            }
        });
        d.present();
    }

    fn remove_dialog(self: &Rc<Self>, src: &Source) {
        let this = self.clone();
        let id = src.id.clone();
        self.confirm(
            "Remove this source?",
            &format!(
                "{}\n\nApps you installed from it stay installed and keep updating from the flake. \
                 They are marked \"source removed\" and can be removed one by one.",
                src.name
            ),
            "Remove",
            move || {
                let mut store = sourceinfo::store();
                store.remove(&id);
                match store.save() {
                    Ok(()) => {
                        // The out-link is a GC root; the next collection frees the catalogue.
                        let _ = std::fs::remove_file(sources::catalogue_link(&id));
                        let _ = std::fs::remove_file(sources::error_file(&id));
                        (this.on_change)();
                    }
                    Err(e) => this.toast(&format!("Could not save the sources: {e}")),
                }
            },
        );
    }

    /// `error` is shown above the entries; the entries keep what was typed.
    pub fn add_dialog(self: &Rc<Self>, name: &str, flake: &str, error: Option<&str>) {
        let d = adw::MessageDialog::new(self.window().as_ref(), Some("Add a Source"), None);
        d.set_heading_use_markup(false);
        d.set_body_use_markup(false);
        d.set_body(
            "A source is a flake that publishes apps. Adding it builds its catalogue, and installing \
             an app builds and runs code from that flake. Only add sources you trust.",
        );
        let boxed = gtk::Box::new(gtk::Orientation::Vertical, 8);
        if let Some(e) = error {
            let l = gtk::Label::new(Some(e));
            l.set_use_markup(false);
            l.set_wrap(true);
            l.add_css_class("error");
            boxed.append(&l);
        }
        let name_entry = gtk::Entry::new();
        name_entry.set_placeholder_text(Some("Name"));
        name_entry.set_text(name);
        let flake_entry = gtk::Entry::new();
        flake_entry.set_placeholder_text(Some("Flake, e.g. github:owner/repo"));
        flake_entry.set_text(flake);
        boxed.append(&name_entry);
        boxed.append(&flake_entry);
        d.set_extra_child(Some(&boxed));
        d.add_response("cancel", "Cancel");
        d.add_response("add", "Add");
        d.set_response_appearance("add", adw::ResponseAppearance::Suggested);
        // Enter must not add a source the user has not read the warning for.
        d.set_default_response(Some("cancel"));
        d.set_close_response("cancel");
        let this = self.clone();
        d.connect_response(None, move |_, r| {
            if r != "add" {
                return;
            }
            let (name, flake) = (name_entry.text().to_string(), flake_entry.text().to_string());
            let mut store = sourceinfo::store();
            match store.add(&name, &flake) {
                Err(e) => this.add_dialog(&name, &flake, Some(&format!("Not a valid flake reference: {e}"))),
                Ok(id) => match store.save() {
                    Ok(()) => {
                        this.rebuild();
                        this.fetch(vec![(id, flake)]);
                    }
                    Err(e) => this.toast(&format!("Could not save the sources: {e}")),
                },
            }
        });
        d.present();
    }

    fn refresh_all(self: &Rc<Self>) {
        let todo = sourceinfo::store()
            .sources()
            .into_iter()
            .filter(|s| !s.builtin && s.enabled)
            .map(|s| (s.id, s.flake))
            .collect::<Vec<_>>();
        if todo.is_empty() {
            self.toast("No sources to refresh");
            return;
        }
        self.fetch(todo);
    }

    /// Builds the catalogues one after the other, then reloads the package list.
    fn fetch(self: &Rc<Self>, todo: Vec<(String, String)>) {
        if self.busy.replace(true) {
            return;
        }
        self.refresh.set_sensitive(false);
        self.refresh.set_label("Refreshing…");
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        relm4::spawn(async move {
            for (id, flake) in todo {
                if let Err(e) = sources::fetch(&id, &flake, sources::FETCH_TIMEOUT).await {
                    log::warn!("source {id}: {e}");
                }
            }
            let _ = tx.send(());
        });
        let this = self.clone();
        relm4::spawn_local(async move {
            let _ = rx.await;
            this.busy.set(false);
            this.refresh.set_sensitive(true);
            this.refresh.set_label("Refresh");
            this.rebuild();
            (this.on_change)();
        });
    }
}
