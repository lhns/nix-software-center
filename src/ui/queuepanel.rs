//! Non-modal progress panel at the bottom of the main window for the install queue.

use super::userops;
use adw::prelude::*;
use nsc_backend::progress::Summary;
use relm4::*;

const MAX_FAILURES: usize = 20;

pub struct QueuePanelModel {
    title: String,
    waiting: usize,
    running: bool,
    cancelling: bool,
    /// Nix reported expected work; `None` pulses the bar instead.
    fraction: Option<f64>,
    statustext: String,
    /// (what failed, Nix's error text), until dismissed.
    failures: Vec<(String, String)>,
    failuretext: String,
}

#[derive(Debug)]
pub enum QueuePanelMsg {
    /// The queue changed; re-read it.
    Refresh,
    Progress(Summary),
    AddFailures(Vec<(String, String)>),
    Cancel,
    Dismiss,
}

#[relm4::component(pub)]
impl SimpleComponent for QueuePanelModel {
    type Init = ();
    type Input = QueuePanelMsg;
    type Output = ();
    type Widgets = QueuePanelWidgets;

    view! {
        gtk::Revealer {
            set_transition_type: gtk::RevealerTransitionType::SlideUp,
            #[watch]
            set_reveal_child: model.running || !model.failures.is_empty(),
            gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                set_spacing: 6,
                set_margin_all: 12,
                add_css_class: "card",
                gtk::Box {
                    set_spacing: 12,
                    set_margin_all: 6,
                    set_orientation: gtk::Orientation::Vertical,
                    gtk::Box {
                        set_spacing: 12,
                        #[watch]
                        set_visible: model.running,
                        gtk::Spinner {
                            set_spinning: true,
                        },
                        gtk::Label {
                            set_hexpand: true,
                            set_halign: gtk::Align::Start,
                            set_ellipsize: gtk::pango::EllipsizeMode::End,
                            add_css_class: "heading",
                            #[watch]
                            set_label: &model.title,
                        },
                        gtk::Label {
                            add_css_class: "dim-label",
                            #[watch]
                            set_visible: model.waiting > 0,
                            #[watch]
                            set_label: &format!("{} more queued", model.waiting),
                        },
                        gtk::Button {
                            set_label: "Cancel",
                            #[watch]
                            set_sensitive: !model.cancelling,
                            connect_clicked[sender] => move |_| {
                                sender.input(QueuePanelMsg::Cancel)
                            },
                        },
                    },
                    #[name(progressbar)]
                    gtk::ProgressBar {
                        #[watch]
                        set_visible: model.running,
                    },
                    gtk::Label {
                        set_halign: gtk::Align::Start,
                        set_ellipsize: gtk::pango::EllipsizeMode::End,
                        add_css_class: "dim-label",
                        #[watch]
                        set_visible: model.running && !model.statustext.is_empty(),
                        #[watch]
                        set_label: &model.statustext,
                    },
                    gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_spacing: 6,
                        #[watch]
                        set_visible: !model.failures.is_empty(),
                        gtk::ScrolledWindow {
                            set_max_content_height: 160,
                            set_propagate_natural_height: true,
                            gtk::Label {
                                set_halign: gtk::Align::Start,
                                set_xalign: 0.0,
                                set_wrap: true,
                                set_selectable: true,
                                add_css_class: "error",
                                #[watch]
                                set_label: &model.failuretext,
                            },
                        },
                        gtk::Button {
                            set_label: "Dismiss",
                            set_halign: gtk::Align::End,
                            connect_clicked[sender] => move |_| {
                                sender.input(QueuePanelMsg::Dismiss)
                            },
                        },
                    },
                },
            },
        }
    }

    fn post_view() {
        if model.running && model.fraction.is_none() {
            progressbar.pulse();
        } else if let Some(f) = model.fraction {
            progressbar.set_fraction(f);
        }
    }

    fn init(
        _: Self::Init,
        root: &Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let model = QueuePanelModel {
            title: String::new(),
            waiting: 0,
            running: false,
            cancelling: false,
            fraction: None,
            statustext: String::new(),
            failures: vec![],
            failuretext: String::new(),
        };
        let widgets = view_output!();
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, _sender: ComponentSender<Self>) {
        match msg {
            QueuePanelMsg::Refresh => {
                let snap = userops::snapshot();
                let title = snap.title.clone().unwrap_or_default();
                if title != self.title || snap.title.is_none() {
                    // A new batch: the old Cancel no longer applies.
                    self.cancelling = false;
                }
                self.running = snap.title.is_some();
                self.title = title;
                self.waiting = snap.waiting;
            }
            QueuePanelMsg::Progress(p) => {
                self.fraction = p.fraction;
                self.statustext = p.label();
            }
            QueuePanelMsg::AddFailures(f) => {
                self.failures.extend(f);
                // Bounded: the panel may stay undismissed for a long time.
                let extra = self.failures.len().saturating_sub(MAX_FAILURES);
                self.failures.drain(..extra);
                self.failuretext = self
                    .failures
                    .iter()
                    .map(|(what, text)| format!("{what}\n{text}"))
                    .collect::<Vec<_>>()
                    .join("\n\n");
            }
            QueuePanelMsg::Cancel => {
                self.cancelling = true;
                userops::cancel_queue_batch();
            }
            QueuePanelMsg::Dismiss => {
                self.failures.clear();
                self.failuretext.clear();
            }
        }
    }
}
