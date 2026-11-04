use super::window::AppMsg;
use adw::prelude::*;
use log::{info, trace};
use nsc_backend::progress::Summary;
use relm4::*;

#[tracker::track]
pub struct RebuildModel {
    hidden: bool,
    #[tracker::do_not_track]
    buffer: gtk::TextBuffer,
    status: RebuildStatus,
    config: String,
    path: String,
    flake: Option<String>,
    /// Nix reported expected work; `None` pulses the bar instead.
    fraction: Option<f64>,
    statustext: String,
    /// A user operation is running that Cancel can stop.
    cancellable: bool,
    cancelling: bool,
}

#[derive(Debug)]
pub enum RebuildMsg {
    Show,
    FinishSuccess,
    FinishError(Option<String>),
    UpdateText(String),
    Progress(Summary),
    SetCancellable(bool),
    Cancel,
    Close,
    Quit,
}

#[derive(PartialEq)]
enum RebuildStatus {
    Building,
    Success,
    Error,
}

#[relm4::component(pub)]
impl SimpleComponent for RebuildModel {
    type Init = gtk::Window;
    type Input = RebuildMsg;
    type Output = AppMsg;
    type Widgets = RebuildWidgets;

    view! {
        dialog = adw::Window {
            set_transient_for: Some(&parent_window),
            set_modal: true,
            #[track(model.changed(RebuildModel::hidden()))]
            set_default_width: 500,
            #[track(model.changed(RebuildModel::hidden()))]
            set_default_height: 200,//295),
            set_resizable: true,
            #[watch]
            set_visible: !model.hidden,
            add_css_class: "dialog",
            add_css_class: "message",
            gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                #[name(statusstack)]
                gtk::Stack {
                    set_margin_top: 20,
                    set_transition_type: gtk::StackTransitionType::Crossfade,
                    set_vhomogeneous: false,
                    #[name(building)]
                    gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_spacing: 10,
                        gtk::Spinner {
                            #[watch]
                            set_spinning: true,
                            set_height_request: 60,
                        },
                        gtk::Label {
                            set_label: "Working…",
                            add_css_class: "title-1",
                        },
                        #[name(progressbar)]
                        gtk::ProgressBar {
                            set_margin_start: 20,
                            set_margin_end: 20,
                        },
                        gtk::Label {
                            #[track(model.changed(RebuildModel::statustext()))]
                            set_label: &model.statustext,
                            #[track(model.changed(RebuildModel::statustext()))]
                            set_visible: !model.statustext.is_empty(),
                            add_css_class: "dim-label",
                            set_ellipsize: gtk::pango::EllipsizeMode::End,
                            set_margin_start: 20,
                            set_margin_end: 20,
                        },
                    },
                    #[name(success)]
                    gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_spacing: 10,
                        gtk::Image {
                            add_css_class: "success",
                            set_icon_name: Some("object-select-symbolic"),
                            set_pixel_size: 128,
                        },
                        gtk::Label {
                            set_label: "Done!",
                            add_css_class: "title-1",
                        },
                    },
                    #[name(error)]
                    gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_spacing: 10,
                        gtk::Image {
                            add_css_class: "error",
                            set_icon_name: Some("dialog-error-symbolic"),
                            set_pixel_size: 128,
                        },
                        gtk::Label {
                            set_label: "Error!",
                            add_css_class: "title-1",
                        },
                        gtk::Label {
                            set_label: "Failed — see output below.",
                            add_css_class: "dim-label",
                        }
                    }
                },
                gtk::Frame {
                    set_margin_all: 20,
                    #[name(scrollwindow)]
                    gtk::ScrolledWindow {
                        set_max_content_height: 500,
                        set_min_content_height: 100,
                        #[name(outview)]
                        gtk::TextView {
                            set_editable: false,
                            set_cursor_visible: false,
                            set_monospace: true,
                            set_top_margin: 5,
                            set_bottom_margin: 5,
                            set_left_margin: 5,
                            set_vexpand: true,
                            set_hexpand: true,
                            set_vscroll_policy: gtk::ScrollablePolicy::Minimum,
                            set_buffer: Some(&buffer),
                        }
                    }
                },
                gtk::Box {
                    add_css_class: "dialog-action-area",
                    set_orientation: gtk::Orientation::Horizontal,
                    set_homogeneous: true,
                    #[track(model.changed(RebuildModel::status()) || model.changed(RebuildModel::cancellable()))]
                    set_visible: model.status != RebuildStatus::Building || model.cancellable,
                    gtk::Button {
                        set_label: "Cancel",
                        #[track(model.changed(RebuildModel::status()) || model.changed(RebuildModel::cancellable()))]
                        set_visible: model.status == RebuildStatus::Building && model.cancellable,
                        #[track(model.changed(RebuildModel::cancelling()))]
                        set_sensitive: !model.cancelling,
                        connect_clicked[sender] => move |_| {
                            sender.input(RebuildMsg::Cancel)
                        }
                    },
                    gtk::Button {
                        set_label: "Close",
                        #[track(model.changed(RebuildModel::status()))]
                        set_visible: model.status != RebuildStatus::Building,
                        connect_clicked[sender] => move |_| {
                            sender.input(RebuildMsg::Close)
                        }
                    }
                }
            }
        }
    }

    fn pre_view() {
        match model.status {
            RebuildStatus::Building => {
                statusstack.set_visible_child(building);
            }
            RebuildStatus::Success => statusstack.set_visible_child(success),
            RebuildStatus::Error => statusstack.set_visible_child(error),
        }
    }

    fn post_view() {
        match model.fraction {
            Some(f) => progressbar.set_fraction(f),
            None => progressbar.pulse(),
        }
        // Follow new lines vertically only; scroll_to_mark would also move the horizontal position.
        let vadj = scrollwindow.vadjustment();
        vadj.set_value(vadj.upper() - vadj.page_size());
        if model.status != RebuildStatus::Building {
            scrollwindow.hadjustment().set_value(0.0);
        }
    }

    fn init(
        parent_window: Self::Init,
        root: &Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {

        let buffer = gtk::TextBuffer::new(None);
        buffer.create_mark(Some("end"), &buffer.end_iter(), false);

        let model = RebuildModel {
            hidden: true,
            buffer: buffer.clone(),
            status: RebuildStatus::Building,
            config: String::new(),
            path: String::new(),
            flake: None,
            fraction: None,
            statustext: String::new(),
            cancellable: false,
            cancelling: false,
            tracker: 0,
        };

        let widgets = view_output!();

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        self.reset();
        match msg {
            RebuildMsg::Show => {
                self.update_hidden(|x| *x = false);
                self.buffer.set_text("");
                self.set_status(RebuildStatus::Building);
                self.set_fraction(None);
                self.set_statustext(String::new());
                self.set_cancellable(false);
                self.set_cancelling(false);
            }
            RebuildMsg::UpdateText(s) => {
                info!("RebuildMsg::UpdateText({})", s);
                self.append(&s);
            }
            RebuildMsg::Progress(p) => {
                self.set_fraction(p.fraction);
                self.set_statustext(p.label());
            }
            RebuildMsg::SetCancellable(c) => {
                self.set_cancellable(c);
            }
            RebuildMsg::Cancel => {
                self.set_cancelling(true);
                crate::ui::userops::cancel_current();
            }
            RebuildMsg::FinishSuccess => {
                self.set_status(RebuildStatus::Success);
            }
            RebuildMsg::FinishError(msg) => {
                if let Some(s) = msg {
                    self.buffer.set_text(&s);
                }
                self.update_hidden(|x| *x = false);
                self.set_status(RebuildStatus::Error);
            }
            RebuildMsg::Close => {
                self.update_hidden(|x| *x = true);
                self.buffer.set_text("");
            }
            RebuildMsg::Quit => {
                sender.output(AppMsg::Close);
            }
        }
    }
}

const MAX_LINES: i32 = 5000;

impl RebuildModel {
    /// One line at the end of the buffer.
    fn append(&self, line: &str) {
        let mut end = self.buffer.end_iter();
        if self.buffer.char_count() > 0 {
            self.buffer.insert(&mut end, "\n");
        }
        self.buffer.insert(&mut end, line);
        // Bounded: a long build can print without end.
        let excess = self.buffer.line_count() - MAX_LINES;
        if excess > 0 {
            let mut from = self.buffer.start_iter();
            let mut to = self.buffer.iter_at_line(excess).unwrap_or_else(|| self.buffer.end_iter());
            self.buffer.delete(&mut from, &mut to);
        }
        trace!("APPENDED: {}", line);
    }
}
