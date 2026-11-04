//! The log modal of the Activity tab: an in-window overlay that looks like an
//! `AdwDialog` (which needs libadwaita 1.5). Hiding it, by its button, a click
//! on the scrim or Esc, never cancels anything; cancelling is a footer button.

use super::{jobs::Job, userops};
use adw::prelude::*;
use nsc_backend::queue::{self, Entry, EntryState};
use relm4::*;
use std::{
    cell::Cell,
    rc::Rc,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

/// Whether the modal is showing, for the window's Esc handler.
static OPEN: AtomicBool = AtomicBool::new(false);

#[derive(Debug)]
pub enum LogMsg {
    Open(u64),
    Hide,
    /// The queue changed.
    Refresh,
    /// Pull new log lines and progress.
    Tick,
    Cancel,
    Retry,
    Remove,
}

pub struct ActivityLogModel {
    root: gtk::Overlay,
    title: adw::WindowTitle,
    hide: gtk::Button,
    status: gtk::Label,
    bar: gtk::ProgressBar,
    pages: gtk::Stack,
    view: gtk::TextView,
    end: gtk::TextMark,
    waiting: adw::StatusPage,
    buffer: gtk::TextBuffer,
    cancel: gtk::Button,
    retry: gtk::Button,
    remove: gtk::Button,
    open: Option<u64>,
    /// Log lines already in the buffer.
    seen: u64,
    /// Whether the view was at the bottom, so new output scrolls along.
    follow: Rc<Cell<bool>>,
    ticking: Rc<Cell<bool>>,
}

/// Esc hides the modal while it is open; `window` sees keys before its content does.
pub fn hide_on_escape(window: &impl IsA<gtk::Widget>) {
    let key = gtk::EventControllerKey::new();
    key.set_propagation_phase(gtk::PropagationPhase::Capture);
    key.connect_key_pressed(|_, key, _, _| {
        if key == gtk::gdk::Key::Escape && OPEN.load(Ordering::Relaxed) {
            super::window::ACTIVITYLOG_BROKER.send(LogMsg::Hide);
            gtk::Inhibit(true)
        } else {
            gtk::Inhibit(false)
        }
    });
    window.add_controller(key);
}

/// Last non-empty line, cut to one readable line.
pub fn last_line(text: &str) -> String {
    let line = text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    let mut short: String = line.chars().take(160).collect();
    if line.chars().count() > 160 {
        short.push('…');
    }
    short
}

impl SimpleComponent for ActivityLogModel {
    type Init = ();
    type Input = LogMsg;
    type Output = ();
    type Root = gtk::Overlay;
    type Widgets = ();

    fn init_root() -> Self::Root {
        let root = gtk::Overlay::new();
        root.set_visible(false);
        root
    }

    fn init(_: (), root: &Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        relm4::set_global_css(
            ".nsc-scrim { background-color: rgba(0, 0, 0, 0.5); }
            .nsc-modal { border-radius: 12px; box-shadow: 0 4px 24px rgba(0, 0, 0, 0.5); }",
        );

        let scrim = gtk::Box::new(gtk::Orientation::Vertical, 0);
        scrim.add_css_class("nsc-scrim");
        scrim.set_hexpand(true);
        scrim.set_vexpand(true);
        root.set_child(Some(&scrim));

        let title = adw::WindowTitle::new("", "");
        let hide = gtk::Button::from_icon_name("window-minimize-symbolic");
        hide.set_tooltip_text(Some("Hide"));
        {
            let sender = sender.clone();
            hide.connect_clicked(move |_| sender.input(LogMsg::Hide));
        }
        let header = adw::HeaderBar::new();
        header.set_show_end_title_buttons(false);
        header.set_show_start_title_buttons(false);
        header.set_title_widget(Some(&title));
        header.pack_end(&hide);

        let status = gtk::Label::new(None);
        status.set_xalign(0.0);
        status.set_ellipsize(gtk::pango::EllipsizeMode::End);
        status.add_css_class("dim-label");
        let bar = gtk::ProgressBar::new();
        let info = gtk::Box::new(gtk::Orientation::Vertical, 8);
        info.set_margin_top(12);
        info.set_margin_bottom(6);
        info.set_margin_start(12);
        info.set_margin_end(12);
        info.append(&status);
        info.append(&bar);

        let buffer = gtk::TextBuffer::new(None);
        let end = buffer.create_mark(None, &buffer.end_iter(), false);
        let view = gtk::TextView::with_buffer(&buffer);
        view.set_editable(false);
        view.set_cursor_visible(false);
        view.set_monospace(true);
        view.set_wrap_mode(gtk::WrapMode::WordChar);
        view.set_top_margin(6);
        view.set_bottom_margin(6);
        view.set_left_margin(8);
        view.set_right_margin(8);
        let scroller = gtk::ScrolledWindow::new();
        scroller.set_child(Some(&view));
        scroller.set_hscrollbar_policy(gtk::PolicyType::Never);
        scroller.set_min_content_height(320);
        scroller.set_vexpand(true);
        scroller.add_css_class("card");
        scroller.set_margin_start(12);
        scroller.set_margin_end(12);

        let follow = Rc::new(Cell::new(true));
        let vadj = scroller.vadjustment();
        {
            let follow = follow.clone();
            vadj.connect_value_changed(move |a| follow.set(a.value() + a.page_size() >= a.upper() - 2.0));
        }
        {
            let follow = follow.clone();
            vadj.connect_upper_notify(move |a| {
                if follow.get() {
                    a.set_value(a.upper() - a.page_size());
                }
            });
        }

        let waiting = adw::StatusPage::new();
        waiting.set_title("Waiting to start");
        waiting.set_icon_name(Some("document-open-recent-symbolic"));
        waiting.add_css_class("compact");
        waiting.set_vexpand(true);
        waiting.set_margin_start(12);
        waiting.set_margin_end(12);
        waiting.set_height_request(320);

        let pages = gtk::Stack::new();
        pages.add_named(&scroller, Some("log"));
        pages.add_named(&waiting, Some("waiting"));

        let button = |label: &str, msg: fn() -> LogMsg| {
            let b = gtk::Button::with_label(label);
            let sender = sender.clone();
            b.connect_clicked(move |_| sender.input(msg()));
            b
        };
        let cancel = button("Cancel", || LogMsg::Cancel);
        let retry = button("Retry", || LogMsg::Retry);
        retry.add_css_class("suggested-action");
        let remove = button("Remove", || LogMsg::Remove);
        let footer = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        footer.set_halign(gtk::Align::End);
        footer.set_margin_top(12);
        footer.set_margin_bottom(12);
        footer.set_margin_start(12);
        footer.set_margin_end(12);
        footer.append(&cancel);
        footer.append(&retry);
        footer.append(&remove);

        let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
        card.add_css_class("background");
        card.add_css_class("nsc-modal");
        card.set_overflow(gtk::Overflow::Hidden);
        card.append(&header);
        card.append(&info);
        card.append(&pages);
        card.append(&footer);

        let clamp = adw::Clamp::new();
        clamp.set_maximum_size(720);
        clamp.set_tightening_threshold(560);
        clamp.set_valign(gtk::Align::Center);
        clamp.set_margin_top(24);
        clamp.set_margin_bottom(24);
        clamp.set_margin_start(24);
        clamp.set_margin_end(24);
        clamp.set_child(Some(&card));
        root.add_overlay(&clamp);
        // A click outside the card hides: the scrim, or the empty sides of the clamp.
        let click = gtk::GestureClick::new();
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        {
            let card: gtk::Widget = card.clone().upcast();
            let (sender, root) = (sender.clone(), root.clone());
            click.connect_released(move |_, _, x, y| {
                let inside = root
                    .pick(x, y, gtk::PickFlags::DEFAULT)
                    .map_or(false, |w| w == card || w.is_ancestor(&card));
                if !inside {
                    sender.input(LogMsg::Hide);
                }
            });
        }
        root.add_controller(click);

        let model = ActivityLogModel {
            root: root.clone(),
            title,
            hide,
            status,
            bar,
            pages,
            view,
            end,
            waiting,
            buffer,
            cancel,
            retry,
            remove,
            open: None,
            seen: 0,
            follow,
            ticking: Rc::new(Cell::new(false)),
        };
        ComponentParts { model, widgets: () }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            LogMsg::Open(id) => {
                if self.open != Some(id) {
                    self.buffer.set_text("");
                    self.seen = 0;
                }
                self.open = Some(id);
                self.follow.set(true);
                OPEN.store(true, Ordering::Relaxed);
                self.root.set_visible(true);
                self.refresh(&sender);
                self.hide.grab_focus();
            }
            LogMsg::Hide => self.close(),
            LogMsg::Refresh | LogMsg::Tick => self.refresh(&sender),
            LogMsg::Cancel => {
                if let Some(id) = self.open {
                    userops::cancel(id);
                }
            }
            LogMsg::Retry => {
                if let Some(id) = self.open {
                    userops::retry(id);
                }
            }
            LogMsg::Remove => {
                if let Some(id) = self.open {
                    userops::remove(id);
                }
                self.close();
            }
        }
    }
}

impl ActivityLogModel {
    fn close(&mut self) {
        self.open = None;
        OPEN.store(false, Ordering::Relaxed);
        self.root.set_visible(false);
    }

    /// Shows the entry's current state and appends the log lines it gained.
    fn refresh(&mut self, sender: &ComponentSender<Self>) {
        let Some(entry) = self.open.and_then(userops::entry) else {
            // Removed from under the modal.
            if self.open.is_some() {
                self.close();
            }
            return;
        };
        self.show(&entry);
        self.append_new(entry.id);
        if entry.state == EntryState::Running && !self.ticking.get() {
            self.ticking.set(true);
            let ticking = self.ticking.clone();
            let sender = sender.input_sender().clone();
            gtk::glib::timeout_add_local(Duration::from_millis(250), move || {
                if !OPEN.load(Ordering::Relaxed) || !userops::has_running() {
                    ticking.set(false);
                    return gtk::glib::Continue(false);
                }
                sender.emit(LogMsg::Tick);
                gtk::glib::Continue(true)
            });
        }
    }

    fn show(&self, entry: &Entry<Job>) {
        self.title.set_title(&entry.payload.title());
        self.title.set_subtitle(entry.payload.target());
        let status = match &entry.state {
            EntryState::Queued => queued_text(entry.ahead),
            EntryState::Running => match &entry.progress {
                Some(p) if !p.label.is_empty() => p.label.clone(),
                _ => "Working…".to_string(),
            },
            EntryState::Done => "Finished".to_string(),
            EntryState::Failed(text) => format!("Failed: {}", last_line(text)),
            EntryState::Cancelled => "Cancelled".to_string(),
        };
        self.status.set_label(&status);
        let running = entry.state == EntryState::Running;
        self.bar.set_visible(running);
        if running {
            match entry.progress.as_ref().and_then(|p| p.fraction) {
                Some(f) => self.bar.set_fraction(f),
                None => self.bar.pulse(),
            }
        }
        let queued = entry.state == EntryState::Queued;
        self.pages.set_visible_child_name(if queued { "waiting" } else { "log" });
        if queued {
            self.waiting.set_description(Some("The log appears once it starts."));
        }
        let finished = matches!(entry.state, EntryState::Done | EntryState::Failed(_) | EntryState::Cancelled);
        let retryable = matches!(entry.state, EntryState::Failed(_) | EntryState::Cancelled);
        self.cancel.set_label(entry.payload.cancel_label());
        self.cancel.set_visible(!finished && userops::can_cancel(entry));
        self.retry.set_visible(retryable);
        self.remove.set_visible(finished);
    }

    fn append_new(&mut self, id: u64) {
        let (lines, seen) = userops::log_since(id, self.seen);
        self.seen = seen;
        if lines.is_empty() {
            return;
        }
        let mut end = self.buffer.end_iter();
        for line in &lines {
            if self.buffer.char_count() > 0 {
                self.buffer.insert(&mut end, "\n");
            }
            self.buffer.insert(&mut end, line);
        }
        // Bounded like the log itself.
        let excess = self.buffer.line_count() - queue::MAX_LOG_LINES as i32;
        if excess > 0 {
            let mut from = self.buffer.start_iter();
            let mut to = self.buffer.iter_at_line(excess).unwrap_or_else(|| self.buffer.end_iter());
            self.buffer.delete(&mut from, &mut to);
        }
        if self.follow.get() {
            // Waits for the text to be laid out, unlike setting the adjustment.
            self.view.scroll_to_mark(&self.end, 0.0, false, 0.0, 1.0);
        }
    }
}

fn queued_text(ahead: usize) -> String {
    match ahead {
        0 => "Up next".to_string(),
        n => format!("Waiting, {n} ahead"),
    }
}
