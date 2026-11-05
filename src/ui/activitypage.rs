//! The Activity tab: running, queued and finished work. Rows open the log
//! modal; their buttons act on the queue directly.

use super::{
    activitylog::{state_text, LogMsg},
    jobs::{Job, JobKind},
    userops,
    window::{AppMsg, ACTIVITYLOG_BROKER},
};
use adw::prelude::*;
use nsc_backend::queue::{Action, Entry, EntryState};
use relm4::*;
use std::{cell::Cell, rc::Rc, time::Duration};

pub struct ActivityPageModel {
    stack: gtk::Stack,
    running: Section,
    queued: Section,
    finished: Section,
    /// Progress bars of the running rows, to move without rebuilding the list.
    bars: Vec<(u64, gtk::ProgressBar, adw::ActionRow)>,
    ticking: Rc<Cell<bool>>,
}

struct Section {
    root: gtk::Box,
    list: gtk::ListBox,
}

impl Section {
    fn new(title: &str, clear: bool) -> Self {
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let label = gtk::Label::new(Some(title));
        label.set_xalign(0.0);
        label.set_hexpand(true);
        label.add_css_class("heading");
        header.append(&label);
        if clear {
            let button = gtk::Button::with_label("Clear Finished");
            button.add_css_class("flat");
            button.set_tooltip_text(Some("Remove all finished entries"));
            button.connect_clicked(|_| userops::clear_finished());
            header.append(&button);
        }
        let list = gtk::ListBox::new();
        list.add_css_class("boxed-list");
        list.set_selection_mode(gtk::SelectionMode::None);
        let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
        root.append(&header);
        root.append(&list);
        Section { root, list }
    }

    fn clear(&self) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
    }
}

#[derive(Debug)]
pub enum ActivityMsg {
    /// The queue changed.
    Refresh,
    /// Move the progress of running rows.
    Tick,
}

impl SimpleComponent for ActivityPageModel {
    type Init = ();
    type Input = ActivityMsg;
    type Output = AppMsg;
    type Root = gtk::Box;
    type Widgets = ();

    fn init_root() -> Self::Root {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_vexpand(true);
        root
    }

    fn init(_: (), root: &Self::Root, _sender: ComponentSender<Self>) -> ComponentParts<Self> {
        let running = Section::new("Running", false);
        let queued = Section::new("Up next", false);
        let finished = Section::new("Finished", true);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 18);
        content.set_valign(gtk::Align::Start);
        content.set_margin_top(18);
        content.set_margin_bottom(18);
        content.set_margin_start(16);
        content.set_margin_end(16);
        content.append(&running.root);
        content.append(&queued.root);
        content.append(&finished.root);
        let clamp = adw::Clamp::new();
        clamp.set_maximum_size(720);
        clamp.set_tightening_threshold(560);
        clamp.set_child(Some(&content));
        let scroller = gtk::ScrolledWindow::new();
        scroller.set_hscrollbar_policy(gtk::PolicyType::Never);
        scroller.set_vexpand(true);
        scroller.set_child(Some(&clamp));

        let empty = adw::StatusPage::new();
        empty.set_title("No Activity");
        empty.set_description(Some("Installs, removals and updates show up here."));
        empty.set_icon_name(Some("view-list-symbolic"));

        let stack = gtk::Stack::new();
        stack.add_named(&scroller, Some("list"));
        stack.add_named(&empty, Some("empty"));
        stack.set_visible_child_name("empty");
        root.append(&stack);

        let model = ActivityPageModel {
            stack,
            running,
            queued,
            finished,
            bars: vec![],
            ticking: Rc::new(Cell::new(false)),
        };
        ComponentParts { model, widgets: () }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            ActivityMsg::Refresh => {
                self.rebuild();
                // Badge and seen marks live in the window.
                sender.output(AppMsg::ActivityChanged);
                if !self.bars.is_empty() && !self.ticking.get() {
                    self.ticking.set(true);
                    let ticking = self.ticking.clone();
                    let sender = sender.input_sender().clone();
                    gtk::glib::timeout_add_local(Duration::from_millis(400), move || {
                        if !userops::has_running() {
                            ticking.set(false);
                            return gtk::glib::Continue(false);
                        }
                        sender.emit(ActivityMsg::Tick);
                        gtk::glib::Continue(true)
                    });
                }
            }
            ActivityMsg::Tick => {
                let entries = userops::entries();
                for (id, bar, row) in &self.bars {
                    if let Some(e) = entries.iter().find(|e| e.id == *id) {
                        set_progress(bar, e);
                        row.set_subtitle(&subtitle(e));
                    }
                }
            }
        }
    }
}

impl ActivityPageModel {
    fn rebuild(&mut self) {
        let entries = userops::entries();
        for s in [&self.running, &self.queued, &self.finished] {
            s.clear();
        }
        self.bars.clear();
        for e in &entries {
            let (row, bar) = row(e);
            let section = match e.state {
                EntryState::Running => &self.running,
                EntryState::Queued => &self.queued,
                _ => &self.finished,
            };
            section.list.append(&row);
            if let Some(bar) = bar {
                self.bars.push((e.id, bar, row));
            }
        }
        let has = |f: fn(&EntryState) -> bool| entries.iter().any(|e| f(&e.state));
        self.running.root.set_visible(has(|s| *s == EntryState::Running));
        self.queued.root.set_visible(has(|s| *s == EntryState::Queued));
        self.finished
            .root
            .set_visible(has(|s| !matches!(s, EntryState::Running | EntryState::Queued)));
        self.stack.set_visible_child_name(if entries.is_empty() { "empty" } else { "list" });
    }
}

fn subtitle(e: &Entry<Job>) -> String {
    let state = state_text(e);
    format!("{} · {}", e.payload.target(), state)
}

fn set_progress(bar: &gtk::ProgressBar, e: &Entry<Job>) {
    match e.progress.as_ref().and_then(|p| p.fraction) {
        Some(f) => bar.set_fraction(f),
        None => bar.pulse(),
    }
}

fn button(label: &str, tooltip: &str, action: impl Fn() + 'static) -> gtk::Button {
    let b = gtk::Button::with_label(label);
    b.set_valign(gtk::Align::Center);
    b.set_tooltip_text(Some(tooltip));
    b.connect_clicked(move |_| action());
    b
}

fn row(e: &Entry<Job>) -> (adw::ActionRow, Option<gtk::ProgressBar>) {
    let id = e.id;
    let row = adw::ActionRow::new();
    // Names are shown as they are: no markup in titles or subtitles.
    row.set_use_markup(false);
    row.set_title(&e.payload.title());
    row.set_title_lines(1);
    row.set_subtitle(&subtitle(e));
    row.set_subtitle_lines(2);
    row.set_activatable(true);
    row.connect_activated(move |_| ACTIVITYLOG_BROKER.send(LogMsg::Open(id)));

    let icon = |name: &str| {
        let i = gtk::Image::from_icon_name(name);
        i.add_css_class("dim-label");
        i
    };
    let mut bar = None;
    match &e.state {
        EntryState::Running => {
            let spinner = gtk::Spinner::new();
            spinner.start();
            row.add_prefix(&spinner);
            let b = gtk::ProgressBar::new();
            b.set_valign(gtk::Align::Center);
            b.set_width_request(120);
            set_progress(&b, e);
            row.add_suffix(&b);
            bar = Some(b);
        }
        EntryState::Queued => row.add_prefix(&icon("document-open-recent-symbolic")),
        EntryState::Done => {
            let i = gtk::Image::from_icon_name("object-select-symbolic");
            i.add_css_class("success");
            row.add_prefix(&i);
        }
        EntryState::Failed(_) => {
            let i = gtk::Image::from_icon_name("dialog-error-symbolic");
            i.add_css_class("error");
            row.add_prefix(&i);
        }
        EntryState::Cancelled => row.add_prefix(&icon("process-stop-symbolic")),
    }

    match e.state {
        EntryState::Queued | EntryState::Running if userops::can_cancel(e) => {
            let tip = match (&e.state, e.action, &e.payload.kind) {
                (EntryState::Queued, ..) => "Remove from the queue",
                (_, Action::Exclusive, JobKind::Update(_)) => "Stop this update",
                (_, Action::Exclusive, _) => "Stop this operation",
                _ => "Stops the whole Nix command: every package in it is cancelled and can be retried",
            };
            row.add_suffix(&button("Cancel", tip, move || userops::cancel(id)));
        }
        EntryState::Failed(_) | EntryState::Cancelled => {
            row.add_suffix(&button("Retry", "Run it again", move || {
                userops::retry(id);
            }));
            row.add_suffix(&button("Remove", "Remove from the list", move || userops::remove(id)));
        }
        EntryState::Done => {
            row.add_suffix(&button("Remove", "Remove from the list", move || userops::remove(id)));
        }
        _ => {}
    }
    (row, bar)
}
