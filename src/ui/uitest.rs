//! CI hook: with `NSC_UI_TEST=<dir>` the app seeds the queue with one entry in
//! each state, walks through the Activity tab and its modal while writing
//! screenshots of its own window to `<dir>`, then quits. No runner starts, so
//! the entries stay as seeded.

use super::{
    activitylog::LogMsg,
    jobs::{Job, JobKind, UpdateJob},
    pkgpage::{InstallType, PkgAction, WorkPkg},
    updatepage::UpdateType,
    userops,
    window::{AppMsg, SystemPkgs, UserPkgs, ACTIVITYLOG_BROKER},
};
use gtk::prelude::*;
use nix_data::config::configfile::NixDataConfig;
use nsc_backend::queue::{Action, BatchResult, Item, Queue};
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

fn id(q: &Queue<Job>, key: &str) -> u64 {
    q.entries().iter().find(|e| e.key == key).map_or(0, |e| e.id)
}

/// Ids of the running, queued and failed entries.
fn seed() -> (u64, u64, u64) {
    let ids = userops::with_queue(|q| {
        add(q, pkg("hello", PkgAction::Install));
        q.start();
        q.log_push("copying path '/nix/store/aaaa-hello-2.12' from 'https://cache.nixos.org'...");
        q.complete(BatchResult::Done);

        add(q, pkg("jq", PkgAction::Install));
        q.start();
        q.log_push("building '/nix/store/bbbb-jq-1.7.drv'...");
        q.log_push("error: builder for '/nix/store/bbbb-jq-1.7.drv' failed with exit code 1");
        q.log_push("<b>&\"markup\"");
        q.complete(BatchResult::Failed("error: builder failed\n<b>&\"markup\"".into()));

        add(q, pkg("ripgrep", PkgAction::Install));
        q.start();
        q.complete(BatchResult::Cancelled);

        add(q, pkg("fd", PkgAction::Install));
        q.start();
        for n in 1..=40 {
            q.log_push(&format!("copying path '/nix/store/cccc{n:02}-fd-9.0.0' from 'https://cache.nixos.org'..."));
        }
        q.set_progress(Some(0.4), "Downloading 12.3 MiB of 30.1 MiB".into());

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

pub fn start(dir: String, window: gtk::Window, input: relm4::Sender<AppMsg>) {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::Relaxed) {
        return;
    }
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("uitest: cannot create {dir}: {e}");
    }
    let (running, queued, failed) = seed();
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
                userops::with_queue(|q| q.complete(BatchResult::Failed("later failure".into())));
                userops::changed();
            }
            14 => shot("06-badge.png"),
            _ => {
                relm4::main_application().quit();
                return gtk::glib::Continue(false);
            }
        }
        gtk::glib::Continue(true)
    });
}
