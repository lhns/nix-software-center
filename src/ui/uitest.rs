//! CI hook: with `NSC_UI_TEST=<dir>` the app seeds the queue with one entry in each state, walks
//! through the Activity tab and its modal while writing screenshots of its own window to `<dir>`,
//! then quits. No runner starts, so the entries stay as seeded. It then walks the Sources page,
//! the Add Source and consent dialogs, the unfree labels and, with a seeded `NSC_SYSTEM_MANIFEST`,
//! the included-with-desktop labels. With `NSC_SYSTEM_PACKAGES=0` it only shows the main window
//! and the preferences.

use super::{
    activitylog::LogMsg,
    jobs::{Job, JobKind, UpdateJob},
    pkgpage::{InstallType, PkgAction, WorkPkg},
    updatepage::UpdateType,
    pkgpage::{self, PkgMsg},
    sourceinfo, userops,
    window::{AppMsg, SystemPkgs, UserPkgs, ACTIVITYLOG_BROKER, PKGPAGE_BROKER},
};
use gtk::prelude::*;
use nsc_backend::sources;
use nix_data::config::configfile::NixDataConfig;
use nsc_backend::queue::{Action, BatchResult, Item, Queue, MAX_SLOTS};
use relm4::gtk;
use std::{
    cell::Cell,
    path::PathBuf,
    rc::Rc,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

fn job(kind: JobKind) -> Job {
    Job {
        kind,
        config: NixDataConfig { systemconfig: None, flake: None, flakearg: None, generations: None },
        syspkgs: SystemPkgs::None,
        userpkgs: UserPkgs::Profile,
    }
}

fn pkg(name: &str, action: PkgAction) -> (String, Action, Job) {
    let work = WorkPkg {
        pkg: name.into(),
        pname: name.into(),
        pkgtype: InstallType::User,
        action: action.clone(),
        block: false,
        notify: None,
    };
    let a = if action == PkgAction::Install { Action::Install } else { Action::Remove };
    (name.into(), a, job(JobKind::Pkg(work)))
}

fn add(q: &mut Queue<Job>, (key, action, payload): (String, Action, Job)) {
    q.enqueue(Item { id: 0, key, action, payload });
}

/// Starts the next item; its id.
fn run(q: &mut Queue<Job>) -> u64 {
    q.start(MAX_SLOTS, true).batch().map_or(0, |b| b.id())
}

fn id(q: &Queue<Job>, key: &str) -> u64 {
    q.entries().iter().find(|e| e.key == key).map_or(0, |e| e.id)
}

/// Ids of the running, queued and failed entries.
fn seed() -> (u64, u64, u64) {
    let ids = userops::with_queue(|q| {
        add(q, pkg("hello", PkgAction::Install));
        let n = run(q);
        q.log_push(n, "copying path '/nix/store/aaaa-hello-2.12' from 'https://cache.nixos.org'...");
        q.complete(n, BatchResult::Done);

        add(q, pkg("jq", PkgAction::Install));
        let n = run(q);
        q.log_push(n, "building '/nix/store/bbbb-jq-1.7.drv'...");
        q.log_push(n, "error: builder for '/nix/store/bbbb-jq-1.7.drv' failed with exit code 1");
        q.log_push(n, "<b>&\"markup\"");
        q.complete(n, BatchResult::Failed("error: builder failed\n<b>&\"markup\"".into()));

        add(q, pkg("ripgrep", PkgAction::Install));
        let n = run(q);
        q.complete(n, BatchResult::Cancelled);

        add(q, pkg("fd", PkgAction::Install));
        let running = run(q);
        for n in 1..=40 {
            q.log_push(running, &format!("copying path '/nix/store/cccc{n:02}-fd-9.0.0' from 'https://cache.nixos.org'..."));
        }
        q.set_progress(running, Some(0.4), "Downloading 12.3 MiB of 30.1 MiB".into());

        add(q, pkg("bat", PkgAction::Remove));
        let update = UpdateJob { scope: UpdateType::All, rm_user: None, rm_sys: None };
        add(q, ("update:all".into(), Action::Exclusive, job(JobKind::Update(update))));
        (id(q, "fd"), id(q, "bat"), id(q, "jq"))
    });
    userops::changed();
    ids
}

fn snapshot(window: &gtk::Window, path: &PathBuf) {
    let (w, h) = (window.width(), window.height());
    let paintable = gtk::WidgetPaintable::new(Some(window));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, w as f64, h as f64);
    let Some(node) = snapshot.to_node() else {
        eprintln!("uitest: nothing to draw for {path:?}");
        return;
    };
    let renderer: Option<gtk::gsk::Renderer> = window.renderer().into();
    let Some(renderer) = renderer else {
        eprintln!("uitest: window has no renderer");
        return;
    };
    let texture = renderer.render_texture(node, None);
    let saved = texture.save_to_png(path);
    println!("uitest: wrote {path:?}: {saved:?}");
}

/// A third-party source whose name and error look like markup, already failed.
fn seed_sources() {
    let mut store = sourceinfo::store();
    if store.sources().len() > 2 {
        return;
    }
    let Ok(id) = store.add("<b>Bold</b> & \"Co\"", "github:example/catalogue") else { return };
    let _ = store.save();
    let _ = std::fs::create_dir_all(sources::error_file(&id).parent().unwrap());
    let _ = std::fs::write(sources::error_file(&id), "error: <b>build</b> of 'x' failed & \"quoted\"
second line");
}

fn toplevel<T: IsA<gtk::Window>>() -> Option<gtk::Window> {
    gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Window>().ok())
        .find(|w| w.is::<T>() && w.is_visible())
}

fn find_button(w: &gtk::Widget, label: &str) -> Option<gtk::Button> {
    if let Some(b) = w.downcast_ref::<gtk::Button>() {
        if b.label().as_deref() == Some(label) {
            return Some(b.clone());
        }
    }
    let mut child = w.first_child();
    while let Some(c) = child {
        if let Some(b) = find_button(&c, label) {
            return Some(b);
        }
        child = c.next_sibling();
    }
    None
}

fn close_all<T: IsA<gtk::Window>>() {
    while let Some(w) = toplevel::<T>() {
        w.close();
        w.destroy();
    }
}

/// Without system packages: the main window and the preferences, which then hold only Sources.
fn start_without_system(dir: String, window: gtk::Window) {
    let dir = PathBuf::from(dir);
    let step = Rc::new(Cell::new(0));
    gtk::glib::timeout_add_local(Duration::from_millis(2000), move || {
        let n = step.get();
        step.set(n + 1);
        match n {
            0 => snapshot(&window, &dir.join("nosys-01-main.png")),
            1 => {
                let _ = window.activate_action("menu.preferences", None);
            }
            2 => {
                if let Some(p) = toplevel::<adw::PreferencesWindow>() {
                    snapshot(&p, &dir.join("nosys-02-preferences.png"));
                } else {
                    eprintln!("uitest: no preferences window");
                }
            }
            _ => {
                relm4::main_application().quit();
                return gtk::glib::Continue(false);
            }
        }
        gtk::glib::Continue(true)
    });
}

pub fn start(dir: String, window: gtk::Window, input: relm4::Sender<AppMsg>) {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::Relaxed) {
        return;
    }
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("uitest: cannot create {dir}: {e}");
    }
    seed_sources();
    if !sources::settings().system_packages {
        return start_without_system(dir, window);
    }
    let (running, queued, failed) = seed();
    let (window2, dir2) = (window.clone(), dir.clone());
    let shot = move |name: &str| snapshot(&window, &PathBuf::from(&dir).join(name));
    let step = Rc::new(Cell::new(0));
    gtk::glib::timeout_add_local(Duration::from_millis(2000), move || {
        let n = step.get();
        step.set(n + 1);
        match n {
            0 => input.emit(AppMsg::ShowTab("activity".into())),
            1 => shot("01-activity.png"),
            2 => ACTIVITYLOG_BROKER.send(LogMsg::Open(running)),
            3 => shot("02-modal-running.png"),
            4 => ACTIVITYLOG_BROKER.send(LogMsg::Hide),
            5 => shot("03-hidden.png"),
            6 => ACTIVITYLOG_BROKER.send(LogMsg::Open(queued)),
            7 => shot("04-modal-queued.png"),
            8 => ACTIVITYLOG_BROKER.send(LogMsg::Hide),
            9 => ACTIVITYLOG_BROKER.send(LogMsg::Open(failed)),
            10 => shot("05-modal-failed.png"),
            11 => ACTIVITYLOG_BROKER.send(LogMsg::Hide),
            12 => input.emit(AppMsg::ShowTab("updates".into())),
            13 => {
                // A failure the user has not seen: the badge gets its attention dot.
                userops::with_queue(|q| {
                    if let Some(id) = q.running().first().map(|b| b.id()) {
                        q.complete(id, BatchResult::Failed("later failure".into()));
                    }
                });
                userops::changed();
            }
            14 => shot("06-badge.png"),
            // Unfree label on a tile.
            15 => {
                input.emit(AppMsg::ShowTab("explore".into()));
                input.emit(AppMsg::UpdateRecPkgs(vec!["unfree-thing".into()]));
            }
            16 => shot("07-tile-unfree.png"),
            // Sources page with both built-ins and the failed third-party source.
            17 => {
                let _ = window2.activate_action("menu.preferences", None);
            }
            18 => {
                if let Some(p) = toplevel::<adw::PreferencesWindow>() {
                    snapshot(&p, &PathBuf::from(&dir2).join("08-sources.png"));
                    if let Some(b) = find_button(p.upcast_ref(), "Add Source…") {
                        b.emit_clicked();
                    }
                }
            }
            19 => {
                if let Some(d) = toplevel::<adw::MessageDialog>() {
                    snapshot(&d, &PathBuf::from(&dir2).join("09-add-source.png"));
                }
                close_all::<adw::MessageDialog>();
                close_all::<adw::PreferencesWindow>();
            }
            // Consent dialog with an app name that looks like markup.
            20 => pkgpage::unfree_dialog("<b>Evil</b> & Co", "Proprietary <i>licence</i>").present(),
            21 => {
                if let Some(d) = toplevel::<adw::MessageDialog>() {
                    snapshot(&d, &PathBuf::from(&dir2).join("10-consent-hostile.png"));
                }
                close_all::<adw::MessageDialog>();
            }
            // The real flow: package page, Install click.
            22 => input.emit(AppMsg::OpenPkg("unfree-thing".into())),
            23 => {
                shot("11-pkgpage-unfree.png");
                PKGPAGE_BROKER.send(PkgMsg::InstallUser);
            }
            24 => {
                if let Some(d) = toplevel::<adw::MessageDialog>() {
                    snapshot(&d, &PathBuf::from(&dir2).join("12-consent-real.png"));
                }
                close_all::<adw::MessageDialog>();
                input.emit(AppMsg::FrontPage);
                input.emit(AppMsg::UpdateInstalledPkgs);
            }
            // Installed rows: unfree, and one whose source was removed.
            25 => input.emit(AppMsg::ShowTab("installed".into())),
            26 => shot("13-installed.png"),
            // Apps in NSC_SYSTEM_MANIFEST (written by CI): "Included with desktop" on the page and a tile.
            27 => input.emit(AppMsg::OpenPkg("hello".into())),
            28 => shot("14-pkgpage-included.png"),
            29 => {
                input.emit(AppMsg::FrontPage);
                input.emit(AppMsg::ShowTab("explore".into()));
                input.emit(AppMsg::UpdateRecPkgs(vec!["hello".into(), "minimal".into(), "awkward".into()]));
            }
            30 => shot("15-tile-included.png"),
            _ => {
                relm4::main_application().quit();
                return gtk::glib::Continue(false);
            }
        }
        gtk::glib::Continue(true)
    });
}
