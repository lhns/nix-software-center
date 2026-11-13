use adw::gio;
use adw::prelude::*;
use html2pango;
use image::{imageops::FilterType, ImageFormat};
use nix_data::config::configfile::NixDataConfig;
use relm4::actions::RelmAction;
use relm4::actions::RelmActionGroup;
use relm4::gtk::pango;
use relm4::{factory::FactoryVecDeque, *};
use sha256::digest;
use std::collections::HashSet;
use std::io::Cursor;
use std::process::Command;
use std::{
    env,
    error::Error,
    fmt::Write,
    fs::{self, File},
    io::BufReader,
    path::Path,
    time::Duration,
};
use log::*;

use crate::parse::packages::PkgMaintainer;
use crate::parse::util;

use nsc_backend::shell;
use super::term::{in_path, launchterm};
use super::jobs::RunJob;
use super::{activitylog::LogMsg, sourceinfo, userops, window::ACTIVITYLOG_BROKER};
use super::window::SystemPkgs;
use super::included;
use super::window::UserPkgs;
use super::{screenshotfactory::ScreenshotItem, window::AppMsg};

#[tracker::track]
#[derive(Debug)]
pub struct PkgModel {
    config: NixDataConfig,
    name: String,
    pkg: String,
    pname: String,
    summary: Option<String>,
    description: Option<String>,
    icon: Option<String>,
    version: Option<String>,

    homepage: Option<String>,
    licenses: Vec<License>,
    platforms: Vec<String>,
    maintainers: Vec<PkgMaintainer>,
    launchable: Option<Launch>,

    syspkgtype: SystemPkgs,
    userpkgtype: UserPkgs,

    #[tracker::no_eq]
    screenshots: FactoryVecDeque<ScreenshotItem>,
    carpage: CarouselPage,
    installtype: InstallType,
    installeduserpkgs: HashSet<String>,
    installedsystempkgs: HashSet<String>,

    visible: bool,
    online: bool,
}

#[derive(Debug, Hash, Eq, PartialEq, Clone)]
pub struct WorkPkg {
    pub pkg: String,
    pub pname: String,
    pub pkgtype: InstallType,
    pub action: PkgAction,
    pub block: bool,
    pub notify: Option<NotifyPage>,
}

impl WorkPkg {
    /// Name for titles and dialogs.
    pub fn display_name(&self) -> &str {
        if self.pname.is_empty() {
            &self.pkg
        } else {
            &self.pname
        }
    }
}

#[derive(Debug, Hash, Eq, PartialEq, Clone)]
pub enum NotifyPage {
    Installed,
}

#[derive(Debug, Hash, Eq, PartialEq, Clone)]
pub enum PkgAction {
    Install,
    Remove
}


#[derive(Debug, PartialEq, Eq)]
pub enum Launch {
    GtkApp(String),
    TerminalApp(String),
}

#[derive(Debug, PartialEq, Eq)]
pub enum CarouselPage {
    First,
    Middle,
    Last,
    Single,
}

#[derive(Debug, Hash, Eq, PartialEq, Clone)]
pub enum InstallType {
    User,
    System,
}

#[derive(Debug, PartialEq, Eq)]
pub struct License {
    pub free: Option<bool>,
    pub fullname: String,
    pub spdxid: Option<String>,
    pub url: Option<String>,
}

#[derive(Debug)]
pub struct PkgInitModel {
    pub name: String,
    pub pkg: String,
    pub installeduserpkgs: HashSet<String>,
    pub installedsystempkgs: HashSet<String>,
    pub pname: String,
    pub summary: Option<String>,
    pub description: Option<String>,
    pub version: Option<String>,
    pub icon: Option<String>,
    pub screenshots: Vec<String>,
    pub homepage: Option<String>,
    pub licenses: Vec<License>,
    pub platforms: Vec<String>,
    pub maintainers: Vec<PkgMaintainer>,
    pub launchable: Option<String>,
}

#[derive(Debug)]
pub enum PkgMsg {
    UpdateConfig(NixDataConfig),
    UpdatePkgTypes(SystemPkgs, UserPkgs),
    Open(Box<PkgInitModel>),
    LoadScreenshot(String, usize, String),
    SetError(String, usize),
    SetCarouselPage(CarouselPage),
    OpenHomepage,
    Close,
    InstallUser,
    RemoveUser,
    InstallSystem,
    RemoveSystem,
    Cancel,
    /// Opens the log of the package's queued or running job.
    ShowDetails,
    Launch,
    NixRun,
    NixShell,
    SetInstallType(InstallType),
    AddToQueue(WorkPkg),
    UpdateOnline(bool),
    /// The app-wide install queue changed: redraw the queue state.
    QueueChanged,
    /// A queued job ended, however it did; `true` on success.
    JobFinished(WorkPkg, bool),
    /// A profile command ended: refresh the installed list once.
    ProfileBatchDone,
}

#[derive(Debug)]
pub enum PkgAsyncMsg {
    LoadScreenshot(String, usize, String),
    SetError(String, usize),
}

#[derive(Debug)]
pub struct PkgPageInit {
    pub syspkgs: SystemPkgs,
    pub userpkgs: UserPkgs,
    pub config: NixDataConfig,
    pub online: bool
}

#[relm4::component(pub)]
impl Component for PkgModel {
    type Init = PkgPageInit;
    type Input = PkgMsg;
    type Output = AppMsg;
    type CommandOutput = PkgAsyncMsg;

    view! {
        #[root]
        #[name(pkg_window)]
        gtk::Box {
            set_orientation: gtk::Orientation::Vertical,
            adw::HeaderBar {
                pack_start = &gtk::Button {
                    add_css_class: "flat",
                    gtk::Image {
                        set_icon_name: Some("go-previous-symbolic"),
                    },
                    connect_clicked[sender] => move |_| {
                        sender.input(PkgMsg::Close)
                    },
                },
                #[wrap(Some)]
                set_title_widget = &gtk::Label {
                    set_ellipsize: pango::EllipsizeMode::End,
                    #[watch]
                    set_label: &model.name
                },
                pack_end = &gtk::MenuButton {
                    #[watch]
                    set_visible: model.syspkgtype != SystemPkgs::None,

                    #[watch]
                    set_label: match model.userpkgtype {
                        UserPkgs::Env => {
                            match model.installtype {
                                InstallType::User => "User (nix-env)",
                                InstallType::System => "System (configuration.nix)",
                            }
                        }
                        UserPkgs::Profile => {
                            match model.installtype {
                                InstallType::User => "User (nix profile)",
                                InstallType::System => "System (configuration.nix)",
                            }
                        }
                    },
                        
                    #[wrap(Some)]
                    set_popover = &gtk::PopoverMenu::from_model(Some(&match model.userpkgtype {
                        UserPkgs::Env => installtype,
                        UserPkgs::Profile => installprofiletype,
                    })) {}
                }
            },
            gtk::ScrolledWindow {
                set_vexpand: true,
                set_hexpand: true,
                set_hscrollbar_policy: gtk::PolicyType::Never,
                set_vscrollbar_policy: gtk::PolicyType::Automatic,
                #[track(model.changed(PkgModel::visible()) && !self.visible)]
                set_vadjustment: gtk::Adjustment::NONE,
                gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,
                    adw::Clamp {
                        set_maximum_size: 1000,
                        set_halign: gtk::Align::Fill,
                        set_valign: gtk::Align::Start,
                        // Details box
                        gtk::Box {
                            set_orientation: gtk::Orientation::Horizontal,
                            // AdwClamp gives a non-expanding child only its natural width, which shrinks with the buttons.
                            set_hexpand: true,
                            set_spacing: 10,
                            set_margin_all: 15,
                            append = if model.icon.is_some() {
                                gtk::Image {
                                    add_css_class: "icon-dropshadow",
                                    set_halign: gtk::Align::Start,
                                    #[watch]
                                    set_from_file: model.icon.clone(),
                                    set_pixel_size: 128,
                                }
                            } else {
                                gtk::Image {
                                    add_css_class: "icon-dropshadow",
                                    set_halign: gtk::Align::Start,
                                    set_icon_name: Some("package-x-generic"),
                                    set_pixel_size: 128,
                                }
                            },
                            gtk::FlowBox {
                                set_halign: gtk::Align::Fill,
                                set_hexpand: true,
                                set_orientation: gtk::Orientation::Horizontal,
                                set_min_children_per_line: 1,
                                set_max_children_per_line: 2,
                                set_selection_mode: gtk::SelectionMode::None,
                                // Details
                                append = &gtk::FlowBoxChild {
                                    set_focusable: false,
                                    add_css_class: "nsc-layout-row",
                                    gtk::Box {
                                        set_halign: gtk::Align::Fill,
                                        set_valign: gtk::Align::Center,
                                        set_hexpand: true,
                                        set_orientation: gtk::Orientation::Vertical,
                                        set_spacing: 6,
                                        gtk::Label {
                                            add_css_class: "title-1",
                                            set_halign: gtk::Align::Start,
                                            set_wrap: true,
                                            set_wrap_mode: pango::WrapMode::WordChar,
                                            set_natural_wrap_mode: gtk::NaturalWrapMode::Word,
                                            #[watch]
                                            set_label: &model.name,
                                        },
                                        gtk::Label {
                                            add_css_class: "dim-label",
                                            add_css_class: "heading",
                                            set_halign: gtk::Align::Start,
                                            set_wrap: true,
                                            set_wrap_mode: pango::WrapMode::WordChar,
                                            set_natural_wrap_mode: gtk::NaturalWrapMode::Word,
                                            #[watch]
                                            set_label: &sourceinfo::caption(&model.pkg),
                                        },
                                        gtk::Label {
                                            add_css_class: "dim-label",
                                            set_halign: gtk::Align::Start,
                                            set_wrap: true,
                                            set_wrap_mode: pango::WrapMode::WordChar,
                                            set_natural_wrap_mode: gtk::NaturalWrapMode::Word,
                                            #[watch]
                                            set_label: &model.version.clone().unwrap_or_else(|| "Unknown".to_string()),
                                        },
                                    },
                                },

                                // Install options
                                append = &gtk::FlowBoxChild {
                                    set_focusable: false,
                                    add_css_class: "nsc-layout-row",
                                    set_halign: gtk::Align::End,
                                    gtk::Box {
                                        set_halign: gtk::Align::End,
                                        set_spacing: 5,
                                        match model.installtype {
                                            InstallType::User => {
                                                gtk::Box {
                                                    #[name(userinstallstack)]
                                                    if userops::state_label(&model.pkg, &InstallType::User, &model.userpkgtype).is_some() {
                                                        gtk::Box {
                                                            set_halign: gtk::Align::End,
                                                            set_spacing: 10,
                                                            gtk::Spinner {
                                                                set_halign: gtk::Align::End,
                                                                #[watch]
                                                                set_spinning: true, //model.installinguserpkgs.contains(&model.pkg),
                                                                set_size_request: (32, 32),
                                                                set_can_focus: false,
                                                            },
                                                            gtk::Label {
                                                                set_valign: gtk::Align::Center,
                                                                add_css_class: "dim-label",
                                                                #[watch]
                                                                set_label: userops::state_label(&model.pkg, &InstallType::User, &model.userpkgtype).unwrap_or(""),
                                                            },
                                                            gtk::Button {
                                                                set_valign: gtk::Align::Center,
                                                                set_can_focus: false,
                                                                set_label: "Details",
                                                                set_tooltip_text: Some("Show the log"),
                                                                connect_clicked[sender] => move |_| {
                                                                    sender.input(PkgMsg::ShowDetails)
                                                                },
                                                            },
                                                            gtk::Button {
                                                                set_halign: gtk::Align::End,
                                                                set_valign: gtk::Align::Center,
                                                                set_can_focus: false,
                                                                set_width_request: 105,
                                                                set_label: "Cancel",
                                                                #[watch]
                                                                set_visible: userops::pkg_can_cancel(&model.pkg, &InstallType::User, &model.userpkgtype),
                                                                connect_clicked[sender] => move |_| {
                                                                    sender.input(PkgMsg::Cancel)
                                                                },
                                                            }
                                                        }                                                   
                                                    } else if model.installeduserpkgs.contains(match model.userpkgtype { UserPkgs::Env => &model.pname, UserPkgs::Profile => &model.pkg }) {
                                                        gtk::Box {
                                                            set_halign: gtk::Align::End,
                                                            set_valign: gtk::Align::Center,
                                                            set_spacing: 10,
                                                            gtk::Button {
                                                                #[watch]
                                                                set_css_classes: if model.launchable.is_some() { &["suggested-action"] } else { &[] },
                                                                set_halign: gtk::Align::End,
                                                                set_valign: gtk::Align::Center,
                                                                set_can_focus: false,
                                                                set_width_request: 105,
                                                                #[watch]
                                                                set_label: if model.launchable.is_some() { "Open" } else { "Installed" },
                                                                #[watch]
                                                                set_sensitive: model.launchable.is_some(),
                                                                connect_clicked[sender] => move |_| {
                                                                    sender.input(PkgMsg::Launch)
                                                                }
                                                            },
                                                            gtk::Button {
                                                                set_halign: gtk::Align::End,
                                                                add_css_class: "destructive-action",
                                                                set_icon_name: "user-trash-symbolic",
                                                                set_can_focus: false,
                                                                connect_clicked[sender] => move |_| {
                                                                    sender.input(PkgMsg::RemoveUser)
                                                                }
                                                            }
                                                        }
                                                    } else if included::shown(&model.pkg, false) {
                                                        gtk::Box {
                                                            set_halign: gtk::Align::End,
                                                            set_valign: gtk::Align::Center,
                                                            set_spacing: 10,
                                                            gtk::Label {
                                                                add_css_class: "dim-label",
                                                                set_label: "Included with desktop",
                                                            },
                                                            gtk::Button {
                                                                add_css_class: "suggested-action",
                                                                set_valign: gtk::Align::Center,
                                                                set_can_focus: false,
                                                                set_label: "Open",
                                                                #[watch]
                                                                set_visible: model.launchable.is_some(),
                                                                connect_clicked[sender] => move |_| {
                                                                    sender.input(PkgMsg::Launch)
                                                                }
                                                            },
                                                        }
                                                    // } else if !model.installinguserpkgs.is_empty() {
                                                    //     gtk::Box {
                                                    //         gtk::Button {
                                                    //             set_halign: gtk::Align::End,
                                                    //             set_valign: gtk::Align::Center,
                                                    //             set_can_focus: false,
                                                    //             set_width_request: 105,
                                                    //             set_label: "Busy",
                                                    //             set_sensitive: false,
                                                    //         }
                                                    //     }
                                                    } else if !model.online {
                                                        gtk::Box {
                                                            set_orientation: gtk::Orientation::Horizontal,
                                                            set_spacing: 10,
                                                            set_halign: gtk::Align::End,
                                                            gtk::Button {
                                                                set_halign: gtk::Align::End,
                                                                set_valign: gtk::Align::Center,
                                                                add_css_class: "error",
                                                                set_label: "Offline",
                                                                set_can_target: false,
                                                            },
                                                            gtk::Button {
                                                                set_halign: gtk::Align::End,
                                                                set_valign: gtk::Align::Center,
                                                                set_icon_name: "nsc-refresh-symbolic",
                                                                connect_clicked[sender] => move |_| {
                                                                    sender.output(AppMsg::CheckNetwork);
                                                                }
                                                            }
                                                        }
                                                    } else {
                                                        adw::SplitButton {
                                                            add_css_class: "suggested-action",
                                                            set_halign: gtk::Align::End,
                                                            set_valign: gtk::Align::Center,
                                                            set_can_focus: false,
                                                            set_label: "Install",
                                                            set_width_request: 105,
                                                            connect_clicked[sender] => move |_| {
                                                                sender.input(PkgMsg::InstallUser);
                                                            },
                                                            // #[watch]
                                                            // set_visible: !model.installeduserpkgs.contains(&model.pname) && !model.installinguserpkgs.contains(&model.pkg),
                                                            #[wrap(Some)]
                                                            set_popover = &gtk::PopoverMenu::from_model(Some(&runaction)) {}
                                                        }
                                                    }
                                                }
                                            }
                                            InstallType::System => {
                                                gtk::Box {
                                                    #[name(systeminstallstack)]
                                                    if userops::state_label(&model.pkg, &InstallType::System, &model.userpkgtype).is_some() {
                                                        gtk::Box {
                                                            set_halign: gtk::Align::End,
                                                            set_spacing: 10,
                                                            gtk::Spinner {
                                                                set_halign: gtk::Align::End,
                                                                #[watch]
                                                                set_spinning: true, //model.installingsystempkgs.contains(&model.pkg),
                                                                set_size_request: (32, 32),
                                                                set_can_focus: false,
                                                            },
                                                            gtk::Label {
                                                                set_valign: gtk::Align::Center,
                                                                add_css_class: "dim-label",
                                                                #[watch]
                                                                set_label: userops::state_label(&model.pkg, &InstallType::System, &model.userpkgtype).unwrap_or(""),
                                                            },
                                                            gtk::Button {
                                                                set_valign: gtk::Align::Center,
                                                                set_can_focus: false,
                                                                set_label: "Details",
                                                                set_tooltip_text: Some("Show the log"),
                                                                connect_clicked[sender] => move |_| {
                                                                    sender.input(PkgMsg::ShowDetails)
                                                                },
                                                            },
                                                            gtk::Button {
                                                                set_halign: gtk::Align::End,
                                                                set_valign: gtk::Align::Center,
                                                                set_can_focus: false,
                                                                set_width_request: 105,
                                                                set_label: "Cancel",
                                                                #[watch]
                                                                set_visible: userops::pkg_can_cancel(&model.pkg, &InstallType::System, &model.userpkgtype),
                                                                connect_clicked[sender] => move |_| {
                                                                    sender.input(PkgMsg::Cancel)
                                                                },
                                                            }
                                                        }                                                   
                                                    } else if model.installedsystempkgs.contains(&model.pkg) {
                                                        gtk::Box {
                                                            set_halign: gtk::Align::End,
                                                            set_valign: gtk::Align::Center,
                                                            set_spacing: 10,
                                                            gtk::Button {
                                                                #[watch]
                                                                set_css_classes: if model.launchable.is_some() { &["suggested-action"] } else { &[] },
                                                                set_halign: gtk::Align::End,
                                                                set_valign: gtk::Align::Center,
                                                                set_can_focus: false,
                                                                set_width_request: 105,
                                                                #[watch]
                                                                set_label: if model.launchable.is_some() { "Open" } else { "Installed" },
                                                                #[watch]
                                                                set_sensitive: model.launchable.is_some(),
                                                                connect_clicked[sender] => move |_| {
                                                                    sender.input(PkgMsg::Launch)
                                                                }
                                                            },
                                                            gtk::Button {
                                                                set_halign: gtk::Align::End,
                                                                add_css_class: "destructive-action",
                                                                set_icon_name: "user-trash-symbolic",
                                                                set_can_focus: false,
                                                                connect_clicked[sender] => move |_| {
                                                                    sender.input(PkgMsg::RemoveSystem)
                                                                }
                                                            }
                                                        }
                                                    // } else if !model.installingsystempkgs.is_empty() {
                                                    //     gtk::Box {
                                                    //         gtk::Button {
                                                    //             set_halign: gtk::Align::End,
                                                    //             set_valign: gtk::Align::Center,
                                                    //             set_can_focus: false,
                                                    //             set_width_request: 105,
                                                    //             set_label: "Busy",
                                                    //             set_sensitive: false,
                                                    //         }
                                                    //     }
                                                    } else if !model.online {
                                                        gtk::Box {
                                                            set_orientation: gtk::Orientation::Horizontal,
                                                            set_spacing: 10,
                                                            set_halign: gtk::Align::End,
                                                            gtk::Button {
                                                                set_halign: gtk::Align::End,
                                                                set_valign: gtk::Align::Center,
                                                                add_css_class: "error",
                                                                set_label: "Offline",
                                                                set_can_target: false,
                                                            },
                                                            gtk::Button {
                                                                set_halign: gtk::Align::End,
                                                                set_valign: gtk::Align::Center,
                                                                set_icon_name: "nsc-refresh-symbolic",
                                                                connect_clicked[sender] => move |_| {
                                                                    sender.output(AppMsg::CheckNetwork);
                                                                }
                                                            }
                                                        }
                                                    } else {
                                                        adw::SplitButton {
                                                            add_css_class: "suggested-action",
                                                            set_halign: gtk::Align::End,
                                                            set_valign: gtk::Align::Center,
                                                            set_can_focus: false,
                                                            set_label: "Install",
                                                            set_width_request: 105,
                                                            connect_clicked[sender] => move |_| {
                                                                sender.input(PkgMsg::InstallSystem);
                                                            },
                                                            // #[watch]
                                                            // set_visible: !model.installedsystempkgs.contains(&model.pname) && !model.installingsystempkgs.contains(&model.pkg),
                                                            #[wrap(Some)]
                                                            set_popover = &gtk::PopoverMenu::from_model(Some(&runaction)) {}
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    },
                    gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_valign: gtk::Align::Start,
                        add_css_class: "view",
                        add_css_class: "frame",
                        add_css_class: "scrnbox",
                        #[watch]
                        set_visible: !model.screenshots.is_empty(),
                        gtk::Overlay {
                            set_valign: gtk::Align::Start,
                            #[local_ref]
                            scrnfactory -> adw::Carousel {
                                set_valign: gtk::Align::Fill,
                                set_hexpand: true,
                                set_vexpand: true,
                                set_height_request: 400,
                                set_allow_scroll_wheel: false,
                                connect_page_changed[sender] => move |x, _| {
                                    let n = adw::Carousel::n_pages(x);
                                    let i = adw::Carousel::position(x) as u32;
                                    if i == 0 && n == 1 {
                                        sender.input(PkgMsg::SetCarouselPage(CarouselPage::Single));
                                    } else if i == 0 {
                                        sender.input(PkgMsg::SetCarouselPage(CarouselPage::First));
                                    } else if i == n - 1 {
                                        sender.input(PkgMsg::SetCarouselPage(CarouselPage::Last));
                                    } else {
                                        sender.input(PkgMsg::SetCarouselPage(CarouselPage::Middle));
                                    }
                                },
                            },
                            add_overlay = &gtk::Revealer {
                                set_transition_type: gtk::RevealerTransitionType::Crossfade,
                                #[watch]
                                set_reveal_child: model.carpage != CarouselPage::First && model.carpage != CarouselPage::Single,
                                set_halign: gtk::Align::Start,
                                set_valign: gtk::Align::Fill,
                                gtk::Button {
                                    set_can_focus: false,
                                    set_margin_all: 15,
                                    set_height_request: 40,
                                    set_width_request: 40,
                                    add_css_class: "circular",
                                    add_css_class: "osd",
                                    set_halign: gtk::Align::Start,
                                    set_valign: gtk::Align::Center,
                                    set_icon_name: "go-previous-symbolic",
                                    connect_clicked[sender, scrnfactory] => move |_| {
                                        let i = adw::Carousel::position(&scrnfactory) as u32;
                                        if i > 0 {
                                            let w = scrnfactory.nth_page(i-1);
                                            scrnfactory.scroll_to(&w, true);
                                        }
                                        if i == 1 {
                                            sender.input(PkgMsg::SetCarouselPage(CarouselPage::First));
                                        } else if i > 0 {
                                            sender.input(PkgMsg::SetCarouselPage(CarouselPage::Middle));
                                        }
                                    }
                                }
                            },
                            add_overlay = &gtk::Revealer {
                                set_transition_type: gtk::RevealerTransitionType::Crossfade,
                                #[watch]
                                set_reveal_child: model.carpage != CarouselPage::Last && model.carpage != CarouselPage::Single,
                                set_halign: gtk::Align::End,
                                set_valign: gtk::Align::Fill,
                                gtk::Button {
                                    set_can_focus: false,
                                    set_margin_all: 15,
                                    set_height_request: 40,
                                    set_width_request: 40,
                                    add_css_class: "circular",
                                    add_css_class: "osd",
                                    set_halign: gtk::Align::End,
                                    set_valign: gtk::Align::Center,
                                    set_icon_name: "go-next-symbolic",
                                    connect_clicked[sender, scrnfactory] => move |_| {
                                        let i = adw::Carousel::position(&scrnfactory) as u32;
                                        if i < scrnfactory.n_pages() -1 {
                                            let w = scrnfactory.nth_page(i+1);
                                            scrnfactory.scroll_to(&w, true);
                                        }
                                        let n = scrnfactory.n_pages() as u32;
                                        if i == n - 2 {
                                            sender.input(PkgMsg::SetCarouselPage(CarouselPage::Last));
                                        } else if i <= n - 2 {
                                            sender.input(PkgMsg::SetCarouselPage(CarouselPage::Middle));
                                        } else {
                                            sender.input(PkgMsg::SetCarouselPage(CarouselPage::Last));
                                        }
                                    }
                                }
                            }
                        },
                        adw::CarouselIndicatorDots {
                            set_halign: gtk::Align::Fill,
                            set_valign: gtk::Align::End,
                            set_carousel: Some(scrnfactory)
                        }
                    },
                    adw::Clamp {
                        set_halign: gtk::Align::Fill,
                        set_valign: gtk::Align::Start,
                        set_vexpand_set: true,
                        set_maximum_size: 1000,
                        #[watch]
                        set_visible: !(model.summary.is_none() && model.description.is_none()),
                        gtk::Box {
                            set_vexpand: true,
                            set_valign: gtk::Align::Start,
                            set_orientation: gtk::Orientation::Vertical,
                            set_margin_all: 15,
                            set_spacing: 10,
                            gtk::Label {
                                add_css_class: "title-2",
                                set_valign: gtk::Align::Start,
                                set_halign: gtk::Align::Start,
                                #[watch]
                                set_label: if let Some(s) = model.summary.as_ref() { s } else { "" },
                                #[watch]
                                set_visible: model.summary.is_some(),
                                set_wrap: true,
                                set_xalign: 0.0,
                            },
                            gtk::Label {
                                set_valign: gtk::Align::Start,
                                set_halign: gtk::Align::Start,
                                #[watch]
                                set_markup: {
                                    if let Some(d) = model.description.as_ref() {
                                        d
                                    } else { "" }
                                },
                                #[watch]
                                set_visible: model.description.is_some(),
                                set_wrap: true,
                                set_xalign: 0.0,
                            },
                        },
                    },
                    adw::Clamp {
                        set_vexpand: true,
                        set_halign: gtk::Align::Fill,
                        set_valign: gtk::Align::Start,
                        set_maximum_size: 1000,
                        #[name(btnbox)]
                        gtk::FlowBox {
                            add_css_class: "linked",
                            set_halign: gtk::Align::Fill,
                            set_hexpand: true,
                            set_margin_bottom: 10,
                            set_homogeneous: true,
                            set_row_spacing: 5,
                            set_column_spacing: 4,
                            set_selection_mode: gtk::SelectionMode::None,
                            set_max_children_per_line: 2,
                            append = &gtk::FlowBoxChild {
                                set_focusable: false,
                                add_css_class: "nsc-layout-row",
                                set_hexpand: true,
                                gtk::Box {
                                    set_spacing: 10,
                                    set_hexpand: true,
                                    set_homogeneous: true,
                                    gtk::Button {
                                        set_hexpand: true,
                                        add_css_class: "card",
                                        set_height_request: 100,
                                        set_width_request: 100,
                                        connect_clicked[sender] => move |_| {
                                            sender.input(PkgMsg::OpenHomepage)
                                        },
                                        gtk::Box {
                                            set_orientation: gtk::Orientation::Vertical,
                                            set_halign: gtk::Align::Fill,
                                            set_valign: gtk::Align::Center,
                                            set_spacing: 10,
                                            set_margin_all: 15,
                                            gtk::Image {
                                                add_css_class: "accent",
                                                set_halign: gtk::Align::Center,
                                                set_icon_name: Some("user-home-symbolic"),
                                                set_pixel_size: 24,
                                            },
                                            gtk::Box {
                                                set_orientation: gtk::Orientation::Vertical,
                                                set_halign: gtk::Align::Fill,
                                                set_valign: gtk::Align::Center,
                                                set_hexpand: true,
                                                set_spacing: 5,
                                                gtk::Label {
                                                    set_halign: gtk::Align::Center,
                                                    set_valign: gtk::Align::Center,
                                                    add_css_class: "heading",
                                                    set_label: "Homepage"
                                                },
                                                gtk::Label {
                                                    set_halign: gtk::Align::Fill,
                                                    set_valign: gtk::Align::Center,
                                                    add_css_class: "caption",
                                                    add_css_class: "dim-label",
                                                    set_ellipsize: pango::EllipsizeMode::End,
                                                    set_lines: 2,
                                                    set_wrap: true,
                                                    set_max_width_chars: 0,
                                                    set_justify: gtk::Justification::Center,
                                                    #[watch]
                                                    set_label: if let Some(u) = &model.homepage {
                                                        u
                                                    } else {
                                                        ""
                                                    },
                                                    #[watch]
                                                    set_visible: model.homepage.is_some(),
                                                }
                                            }

                                        }
                                    },
                                    gtk::Button {
                                        set_hexpand: true,
                                        add_css_class: "card",
                                        set_height_request: 100,
                                        set_width_request: 100,
                                        gtk::Box {
                                            set_orientation: gtk::Orientation::Vertical,
                                            set_halign: gtk::Align::Fill,
                                            set_valign: gtk::Align::Center,
                                            set_spacing: 10,
                                            set_margin_all: 15,
                                            gtk::Image {
                                                #[watch]
                                                set_css_classes: &[ if model.licenses.iter().any(|x| x.free == Some(false)) { "error" } else if (!model.licenses.is_empty() && model.licenses.iter().all(|x| x.free == Some(true))) { "success" } else { "warning" } ],
                                                set_halign: gtk::Align::Center,
                                                #[watch]
                                                set_icon_name : if model.licenses.iter().any(|x| x.free == Some(false)) { Some("dialog-warning-symbolic") } else if (!model.licenses.is_empty() && model.licenses.iter().all(|x| x.free == Some(true))) { Some("emblem-default-symbolic") } else { Some("dialog-question-symbolic") },
                                                set_pixel_size: 24,
                                            },
                                            gtk::Box {
                                                set_orientation: gtk::Orientation::Vertical,
                                                set_halign: gtk::Align::Fill,
                                                set_valign: gtk::Align::Center,
                                                set_spacing: 5,
                                                gtk::Label {
                                                    set_halign: gtk::Align::Center,
                                                    add_css_class: "heading",
                                                    #[watch]
                                                    set_label: if model.licenses.len() > 1 { "Licenses" } else { "License" }
                                                },
                                                gtk::Label {
                                                    set_halign: gtk::Align::Fill,
                                                    set_hexpand: true,
                                                    add_css_class: "caption",
                                                    add_css_class: "dim-label",
                                                    set_ellipsize: pango::EllipsizeMode::End,
                                                    set_lines: 2,
                                                    set_wrap: true,
                                                    set_max_width_chars: 0,
                                                    set_justify: gtk::Justification::Center,
                                                    #[watch]
                                                    set_label: {
                                                        let mut s = String::new();
                                                        for license in model.licenses.iter() {
                                                            if model.licenses.iter().len() == 1 {
                                                                if let Some(id) = &license.spdxid {
                                                                    s.push_str(id)
                                                                } else {
                                                                    s.push_str(&license.fullname)
                                                                }
                                                            } else if model.licenses.iter().len() == 2 && model.licenses.get(0) == Some(license) {
                                                                if let Some(id) = &license.spdxid {
                                                                    let _ = write!(s, "{} ", id);
                                                                } else {
                                                                    let _ = write!(s, "{} ", license.fullname);
                                                                }
                                                            } else if Some(license) == model.licenses.iter().last() {
                                                                if let Some(id) = &license.spdxid {
                                                                    let _ = write!(s, "and {}", id);
                                                                } else {
                                                                    let _ = write!(s, "and {}", license.fullname);
                                                                }
                                                            } else if let Some(id) = &license.spdxid {
                                                                let _ = write!(s, "{}, ", id);
                                                            } else {
                                                                let _ = write!(s, "{}, ", license.fullname);
                                                            }
                                                        }
                                                        if model.licenses.is_empty() {
                                                            s.push_str("Unknown");
                                                        }
                                                        &s.to_string()
                                                    },
                                                    #[watch]
                                                    set_visible: !model.licenses.is_empty()
                                                }
                                            }
                                        }
                                    },
                                }
                            },
                            append = &gtk::FlowBoxChild {
                                set_focusable: false,
                                add_css_class: "nsc-layout-row",
                                set_hexpand: true,
                                gtk::Box {
                                    set_spacing: 10,
                                    set_hexpand: true,
                                    set_homogeneous: true,
                                    gtk::Button {
                                        set_hexpand: true,
                                        add_css_class: "card",
                                        set_height_request: 100,
                                        set_width_request: 100,
                                        gtk::Box {
                                            set_orientation: gtk::Orientation::Vertical,
                                            set_valign: gtk::Align::Center,
                                            set_spacing: 10,
                                            set_margin_all: 15,
                                            gtk::Image {
                                                add_css_class: "success",
                                                set_icon_name: Some("video-display-symbolic"),
                                                set_pixel_size: 24,
                                            },
                                            gtk::Box {
                                                set_orientation: gtk::Orientation::Vertical,
                                                set_valign: gtk::Align::Center,
                                                set_spacing: 5,
                                                gtk::Label {
                                                    set_halign: gtk::Align::Center,
                                                    add_css_class: "heading",
                                                    set_label: "Platforms"
                                                },
                                                gtk::Label {
                                                    set_halign: gtk::Align::Fill,
                                                    set_hexpand: true,
                                                    add_css_class: "caption",
                                                    add_css_class: "dim-label",
                                                    set_ellipsize: pango::EllipsizeMode::End,
                                                    set_lines: 2,
                                                    set_wrap: true,
                                                    set_max_width_chars: 0,
                                                    set_justify: gtk::Justification::Center,
                                                    #[watch]
                                                    set_label: {
                                                        let mut s = String::new();
                                                        for p in model.platforms.iter() {
                                                            if model.platforms.iter().len() == 1 {
                                                                s.push_str(p);
                                                            } else if model.platforms.iter().len() == 2 && model.platforms.get(0) == Some(p) {
                                                                let _ = write!(s, "{} ", p);
                                                            } else if Some(p) == model.platforms.iter().last() {
                                                                let _ = write!(s, "and {}", p);
                                                            } else {
                                                                let _ = write!(s, "{}, ", p);
                                                            }
                                                        }
                                                        if model.platforms.is_empty() {
                                                            s.push_str("Unknown");
                                                        }
                                                        &s.to_string()
                                                    },
                                                    #[watch]
                                                    set_visible: !model.platforms.is_empty()
                                                }
                                            }
                                        }
                                    },
                                    gtk::Button {
                                        set_hexpand: true,
                                        add_css_class: "card",
                                        set_height_request: 100,
                                        set_width_request: 100,
                                        gtk::Box {
                                            set_orientation: gtk::Orientation::Vertical,
                                            set_halign: gtk::Align::Fill,
                                            set_valign: gtk::Align::Center,
                                            set_spacing: 10,
                                            set_margin_all: 15,
                                            gtk::Image {
                                                add_css_class: "circular",
                                                #[watch]
                                                set_css_classes: &[ if model.maintainers.is_empty() { "error" } else { "accent" } ],
                                                set_halign: gtk::Align::Center,
                                                set_icon_name: Some("system-users-symbolic"),
                                                set_pixel_size: 24,
                                            },
                                            gtk::Box {
                                                set_orientation: gtk::Orientation::Vertical,
                                                set_valign: gtk::Align::Center,
                                                set_spacing: 5,
                                                gtk::Label {
                                                    set_halign: gtk::Align::Center,
                                                    add_css_class: "heading",
                                                    #[watch]
                                                    set_label: if model.maintainers.len() > 1 { "Maintainers" } else { "Maintainer" }
                                                },
                                                gtk::Label {
                                                    set_halign: gtk::Align::Fill,
                                                    set_hexpand: true,
                                                    add_css_class: "caption",
                                                    add_css_class: "dim-label",
                                                    set_ellipsize: pango::EllipsizeMode::End,
                                                    set_lines: 2,
                                                    set_wrap: true,
                                                    set_max_width_chars: 0,
                                                    set_justify: gtk::Justification::Center,
                                                    #[watch]
                                                    set_label: {
                                                        let mut s = String::new();
                                                        let maintainerlist = model.maintainers.iter().filter(|m| m.name.is_some() || m.github.is_some()).collect::<Vec<_>>();
                                                        for p in &maintainerlist {
                                                            if maintainerlist.len() == 1 {
                                                                if let Some(n) = &p.name {
                                                                    s.push_str(n);
                                                                } else if let Some(g) = &p.github {
                                                                    s.push_str(g);
                                                                }
                                                            } else if maintainerlist.len() == 2 && model.maintainers.get(0) == Some(p) {
                                                                if let Some(n) = &p.name {
                                                                    let _ = write!(s, "{} ", n.as_str());
                                                                } else if let Some(g) = &p.github {
                                                                    s.push_str(g);
                                                                }
                                                            } else if Some(p) == maintainerlist.last() {
                                                                if let Some(n) = &p.name {
                                                                    let _ = write!(s, "and {}", n.as_str());
                                                                } else if let Some(g) = &p.github {
                                                                    let _ = write!(s, "and {}", g.as_str());
                                                                }
                                                            } else if let Some(n) = &p.name {
                                                                let _ = write!(s, "{}, ", n.as_str());
                                                            } else if let Some(g) = &p.github {
                                                                let _ = write!(s, "{}, ", g.as_str());
                                                            }
                                                        }
                                                        if model.maintainers.is_empty() {
                                                            s.push_str("Unknown");
                                                        }
                                                        &s.to_string()
                                                    }
                                                }
                                            }
                                        }
                                    },
                                }
                            },
                        }
                    },
                    gtk::Separator {
                        set_vexpand: true,
                        add_css_class: "spacer"
                    }
                }
            }
        }
    }

    menu! {
        installtype: {
            "User (nix-env)" => NixEnvAction,
            "System (configuration.nix)" => NixSystemAction,
        },
        installprofiletype: {
            "User (nix-profile)" => NixProfileAction,
            "System (configuration.nix)" => NixSystemAction,
        },
        runaction: {
            "Run without installing" => LaunchAction,
            "Open interactive shell" => TermShellAction,
        }
    }

    fn init(
        initparams: Self::Init,
        root: &Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let config = initparams.config;
        let model = PkgModel {
            config,
            name: String::default(),
            pkg: String::default(),
            pname: String::default(),
            summary: None,
            description: None,
            version: None,
            icon: None,
            homepage: None,
            licenses: vec![],
            screenshots: FactoryVecDeque::new(adw::Carousel::new(), sender.input_sender()),
            platforms: vec![],
            carpage: CarouselPage::Single,
            installtype: InstallType::User,
            maintainers: vec![],
            installeduserpkgs: HashSet::new(),
            installedsystempkgs: HashSet::new(),
            syspkgtype: initparams.syspkgs,
            userpkgtype: initparams.userpkgs,
            launchable: None,
            visible: false,
            online: initparams.online,
            tracker: 0,
        };

        let scrnfactory = model.screenshots.widget();
        relm4::set_global_css(
            ".scrnbox {
            border-left-width: 0;
            border-right-width: 0;
            border-top-width: 1px;
            border-bottom-width: 1px;
        }
            flowboxchild.nsc-layout-row,
            flowboxchild.nsc-layout-row:hover,
            flowboxchild.nsc-layout-row:active {
            background: none;
        }",
        );
        let widgets = view_output!();
        widgets.userinstallstack.set_hhomogeneous(false);
        widgets.systeminstallstack.set_hhomogeneous(false);
        // A non-expanding stack sits at the start of its wrapper Box, which left Install short of the right edge.
        widgets.userinstallstack.set_hexpand(true);
        widgets.systeminstallstack.set_hexpand(true);

        let mut group = RelmActionGroup::<ModeActionGroup>::new();
        let nixenv: RelmAction<NixEnvAction> = {
            let sender = sender.clone();
            RelmAction::new_stateless(move |_| {
                sender.input(PkgMsg::SetInstallType(InstallType::User));
            })
        };

        let nixprofile: RelmAction<NixProfileAction> = {
            let sender = sender.clone();
            RelmAction::new_stateless(move |_| {
                sender.input(PkgMsg::SetInstallType(InstallType::User));
            })
        };

        let nixsystem: RelmAction<NixSystemAction> = {
            let sender = sender.clone();
            RelmAction::new_stateless(move |_| {
                sender.input(PkgMsg::SetInstallType(InstallType::System));
            })
        };

        group.add_action(nixenv);
        group.add_action(nixprofile);
        group.add_action(nixsystem);

        let actions = group.into_action_group();
        widgets
            .pkg_window
            .insert_action_group("mode", Some(&actions));

        let mut rungroup = RelmActionGroup::<RunActionGroup>::new();
        let launchaction: RelmAction<LaunchAction> = {
            let sender = sender.clone();
            RelmAction::new_stateless(move |_| {
                sender.input(PkgMsg::NixRun);
            })
        };

        let termaction: RelmAction<TermShellAction> = {
            let sender = sender;
            RelmAction::new_stateless(move |_| {
                sender.input(PkgMsg::NixShell)
            })
        };

        rungroup.add_action(launchaction);
        rungroup.add_action(termaction);

        let runactions = rungroup.into_action_group();
        widgets
            .pkg_window
            .insert_action_group("run", Some(&runactions));

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>, _root: &Self::Root) {
        self.reset();
        match msg {
            PkgMsg::UpdateConfig(config) => {
                self.config = config.clone();
            }
            PkgMsg::UpdatePkgTypes(syspkgs, userpkgs) => {
                self.syspkgtype = syspkgs.clone();
                self.userpkgtype = userpkgs.clone();
            }
            PkgMsg::Open(pkgmodel) => {

                // First clean up from previous package
                self.summary = None;
                self.description = None;
                self.icon = None;
                let mut scrn_guard = self.screenshots.guard();
                scrn_guard.clear();
                scrn_guard.drop();

                self.set_visible(true);
                self.set_pkg(pkgmodel.pkg);
                self.set_name(pkgmodel.name);
                self.set_icon(pkgmodel.icon);
                self.set_version(pkgmodel.version);
                self.set_platforms(pkgmodel.platforms);
                self.set_maintainers(pkgmodel.maintainers);
                self.set_licenses(pkgmodel.licenses);
                self.set_pname(pkgmodel.pname);
                self.set_installeduserpkgs(pkgmodel.installeduserpkgs);
                self.set_installedsystempkgs(pkgmodel.installedsystempkgs);

                if self.installedsystempkgs.contains(&self.pkg) && !self.installeduserpkgs.contains(match self.userpkgtype { UserPkgs::Env => &self.pname, UserPkgs::Profile => &self.pkg }) {
                    self.set_installtype(InstallType::System)
                } else {
                    self.set_installtype(InstallType::User)
                }

                self.launchable = if let Some(l) = pkgmodel.launchable {
                    Some(Launch::GtkApp(l))
                } else if self.installeduserpkgs.contains(match self.userpkgtype { UserPkgs::Env => &self.pname, UserPkgs::Profile => &self.pkg }) {
                    if in_path(&self.pname) {
                        Some(Launch::TerminalApp(self.pname.to_string()))
                    } else {
                        None
                    }
                } else {
                    None
                };
                self.summary = if let Some(s) = pkgmodel.summary {
                    let mut sum = s.trim().to_string();
                    while sum.contains('\n') {
                        sum = sum.replace('\n', " ");
                    }
                    while sum.contains("  ") {
                        sum = sum.replace("  ", " ");
                    }
                    Some(sum)
                } else {
                    None
                };

                if let Some(d) = pkgmodel.description {
                    let mut input = d;
                    // Fix formatting
                    while input.contains('\n') {
                        input = input.replace('\n', " ");
                    }
                    while input.contains('\t') {
                        input = input.replace('\t', " ");
                    }
                    while input.contains("  ") {
                        input = input.replace("  ", " ");
                    }
                    // A third-party description is text, never markup (no tags, no links).
                    let third_party = self.pkg.contains(':');
                    let mut pango = if third_party {
                        Ok(gtk::glib::markup_escape_text(&input).to_string())
                    } else {
                        html2pango::markup_html(&input)
                    }
                        .unwrap_or_else(|_| {
                            warn!("Pango failed to parse description");
                            // Goes to set_markup: unparsed text must not be read as markup.
                            gtk::glib::markup_escape_text(&input).to_string()
                        })
                        .trim()
                        .to_string();
                    while pango.contains("\n ") {
                        pango = pango.replace("\n ", "\n");
                    }
                    while pango.ends_with('\n') {
                        pango.pop();
                    }
                    self.description = Some(pango.strip_prefix('\n').unwrap_or(&pango).to_string());
                }

                self.homepage = pkgmodel.homepage;

                if pkgmodel.screenshots.len() <= 1 {
                    self.carpage = CarouselPage::Single;
                } else {
                    self.carpage = CarouselPage::First;
                }

                {
                    let mut scrn_guard = self.screenshots.guard();
                    scrn_guard.clear();
                    for _i in 0..pkgmodel.screenshots.len() {
                        scrn_guard.push_back(());
                    }
                }

                for (i, url) in pkgmodel.screenshots.into_iter().enumerate() {
                    if let Ok(home) = env::var("HOME") {
                        let cachedir = format!("{}/.cache/nix-software-center", home);
                        let sha = digest(url.to_string());
                        let scrnpath = format!("{}/screenshots/{}", cachedir, sha);
                        let pkg = self.pkg.clone();

                        sender.command(move |out, shutdown| {
                            let url = url.clone();
                            let home = home.clone();
                            let scrnpath = scrnpath.clone();
                            let pkg = pkg.clone();
                            shutdown
                                .register(async move {
                                    tokio::time::sleep(Duration::from_millis(5)).await;
                                    if Path::new(&format!("{}.png", scrnpath)).exists() {
                                        out.send(PkgAsyncMsg::LoadScreenshot(pkg, i, format!("{}.png", scrnpath)));
                                    } else {
                                        match reqwest::get(&url).await {
                                            Ok(response) => {
                                                if response.status().is_success() {
                                                    if !Path::new(&format!(
                                                        "{}/.cache/nix-software-center/screenshots",
                                                        home
                                                    ))
                                                    .exists()
                                                    {
                                                        match fs::create_dir_all(format!(
                                                            "{}/.cache/nix-software-center/screenshots",
                                                            home
                                                        )) {
                                                            Ok(_) => {}
                                                            Err(_) => {
                                                                out.send(PkgAsyncMsg::SetError(pkg, i));
                                                                return;
                                                            }
                                                        }
                                                    }
                                                    if let Ok(mut file) = File::create(&scrnpath) {
                                                        if let Ok(b) = response.bytes().await {
                                                            let mut content =  Cursor::new(b);
                                                            if std::io::copy(&mut content, &mut file).is_ok() {
                                                                fn openimg(scrnpath: &str) -> Result<(), Box<dyn Error>> {
                                                                    let img = if let Ok(x) = image::load(BufReader::new(File::open(scrnpath)?), image::ImageFormat::Png) {
                                                                        x
                                                                    } else if let Ok(x) = image::load(BufReader::new(File::open(scrnpath)?), image::ImageFormat::Jpeg) {
                                                                        x
                                                                    } else if let Ok(x) = image::load(BufReader::new(File::open(scrnpath)?), image::ImageFormat::WebP) {
                                                                        x
                                                                    } else {
                                                                        let imgdata = BufReader::new(File::open(scrnpath)?);
                                                                        let format = image::guess_format(imgdata.buffer())?;
                                                                        image::load(imgdata, format)?
                                                                    };
                                                                    let scaled = img.resize(640, 360, FilterType::Lanczos3);
                                                                    let mut output = File::create(&format!("{}.png", scrnpath))?;
                                                                    scaled.write_to(&mut output, ImageFormat::Png)?;
                                                                    if let Err(e) = fs::remove_file(&scrnpath) {
                                                                        warn!("{}", e);
                                                                    }
                                                                    Ok(())
                                                                }
    
                                                                match openimg(&scrnpath) {
                                                                    Ok(_) => {
                                                                        out.send(PkgAsyncMsg::LoadScreenshot(
                                                                            pkg, i, format!("{}.png", scrnpath),
                                                                        ));
                                                                    }
                                                                    Err(_) => {
                                                                        if let Err(e) = fs::remove_file(&scrnpath) {
                                                                            warn!("{}", e);
                                                                        }
                                                                        out.send(PkgAsyncMsg::SetError(pkg, i));
                                                                    }
                                                                }
                                                            }
                                                        }
                                                    } else {
                                                        out.send(PkgAsyncMsg::SetError(pkg, i));
                                                        warn!("Error: {}", response.status());    
                                                    }
                                                } else {
                                                    out.send(PkgAsyncMsg::SetError(pkg, i));
                                                    warn!("Error: {}", response.status());
                                                }
                                            }
                                            Err(e) => {
                                                out.send(PkgAsyncMsg::SetError(pkg, i));
                                                warn!("Error: {}", e);
                                            }
                                        }
                                    }
                                })
                                .drop_on_shutdown()
                        })
                    }
                }
            }
            PkgMsg::LoadScreenshot(pkg, i, u) => {
                info!("PkgMsg::LoadScreenshot {}", u);
                if pkg == self.pkg {
                    let mut scrn_guard = self.screenshots.guard();
                    if let Some(mut scrn_widget) = scrn_guard.get_mut(i) {
                        scrn_widget.path = Some(u);
                        trace!("GOT PATH")
                    } else {
                        trace!("NO SCRN WIDGET")
                    }
                } else {
                    trace!("WRONG PACKAGE")
                }
            }
            PkgMsg::SetError(pkg, i) => {
                if pkg == self.pkg {
                    let mut scrn_guard = self.screenshots.guard();
                    if let Some(mut scrn_widget) = scrn_guard.get_mut(i) {
                        scrn_widget.error = true;
                    }
                }
            }
            PkgMsg::SetCarouselPage(page) => {
                self.carpage = page;
            }
            PkgMsg::OpenHomepage => {
                if let Some(u) = &self.homepage {
                    if let Err(e) =
                        gio::AppInfo::launch_default_for_uri(u, gio::AppLaunchContext::NONE)
                    {
                        warn!("error: {}", e);
                    }
                }
            }
            PkgMsg::Close => {
                self.set_visible(false);
                sender.output(AppMsg::FrontPage);
            }
            PkgMsg::InstallUser => {
                let online = util::checkonline();
                if !online {
                    sender.output(AppMsg::CheckNetwork);
                    self.online = false;
                    return;
                }
                let w = WorkPkg {
                    pkg: self.pkg.to_string(),
                    pname: self.pname.to_string(),
                    pkgtype: InstallType::User,
                    action: PkgAction::Install,
                    block: false,
                    notify: None,
                };
                self.queue_work(w, &sender);
            }
            PkgMsg::RemoveUser => {
                let w = WorkPkg {
                    pkg: self.pkg.to_string(),
                    pname: self.pname.to_string(),
                    pkgtype: InstallType::User,
                    action: PkgAction::Remove,
                    block: false,
                    notify: None,
                };
                self.queue_work(w, &sender);
            }
            PkgMsg::InstallSystem => {
                let online = util::checkonline();
                if !online {
                    sender.output(AppMsg::CheckNetwork);
                    self.online = false;
                    return;
                }
                let w = WorkPkg {
                    pkg: self.pkg.to_string(),
                    pname: self.pname.to_string(),
                    pkgtype: InstallType::System,
                    action: PkgAction::Install,
                    block: false,
                    notify: None,
                };
                self.queue_work(w, &sender);
            }
            PkgMsg::RemoveSystem => {
                let w = WorkPkg {
                    pkg: self.pkg.to_string(),
                    pname: self.pname.to_string(),
                    pkgtype: InstallType::System,
                    action: PkgAction::Remove,
                    block: false,
                    notify: None,
                };
                self.queue_work(w, &sender);
            }
            PkgMsg::Cancel => {
                if let Some(id) = userops::active_id(&self.pkg, &self.installtype, &self.userpkgtype) {
                    userops::cancel(id);
                }
            }
            PkgMsg::ShowDetails => {
                if let Some(id) = userops::active_id(&self.pkg, &self.installtype, &self.userpkgtype) {
                    ACTIVITYLOG_BROKER.send(LogMsg::Open(id));
                }
            }
            PkgMsg::Launch => {
                if let Some(l) = &self.launchable {
                    match l {
                        Launch::GtkApp(x) => {
                            let _ = Command::new("gtk-launch").arg(x).spawn();
                        }
                        Launch::TerminalApp(x) => {
                            launchterm(x);
                        }
                    }
                }
            }
            PkgMsg::NixRun => match self.userpkgtype {
                UserPkgs::Env => match &self.launchable {
                    Some(Launch::GtkApp(x)) => {
                        debug!("Launching {} with nix-shell", x);
                        let _ = Command::new("nix-shell")
                            .arg("-p")
                            .arg(&self.pkg)
                            .arg("--command")
                            .arg(&format!("XDG_DATA_DIRS=$XDG_DATA_DIRS:$buildInputs/share gtk-launch {}", shell::quote(x)))
                            .spawn();
                    }
                    Some(Launch::TerminalApp(x)) => launchterm(&shell::terminal_run(&self.pkg, x)),
                    None => launchterm(&shell::terminal_run(&self.pkg, &self.pname)),
                },
                UserPkgs::Profile => {
                    if !util::checkonline() {
                        sender.output(AppMsg::CheckNetwork);
                        self.online = false;
                        return;
                    }
                    let desktop_id = match &self.launchable {
                        Some(Launch::GtkApp(id)) => Some(id.clone()),
                        _ => None,
                    };
                    let job = RunJob { pkg: self.pkg.to_string(), pname: self.pname.to_string(), desktop_id };
                    self.queue_run(job);
                }
            },
            PkgMsg::NixShell => {
                let quoted = match sourceinfo::installable(&self.pkg) {
                    Ok(i) => shell::quote(&i),
                    Err(e) => {
                        warn!("Cannot open a shell with {}: {}", self.pkg, e);
                        return;
                    }
                };
                let cmd = match self.userpkgtype {
                    UserPkgs::Env => {
                        format!("nix-shell -p {}", shell::quote(&self.pkg))
                    }
                    UserPkgs::Profile => {
                        format!("nix shell {}", quoted)
                    }
                };
                launchterm(&cmd);
            }
            PkgMsg::SetInstallType(t) => {
                self.set_installtype(t);
            }
            PkgMsg::AddToQueue(work) => {
                self.queue_work(work, &sender);
            }
            PkgMsg::QueueChanged => {}
            PkgMsg::JobFinished(work, ok) => {
                if ok {
                    let name = match (&work.pkgtype, &self.userpkgtype) {
                        (InstallType::User, UserPkgs::Env) => work.pname.to_string(),
                        _ => work.pkg.to_string(),
                    };
                    let installed = match work.pkgtype {
                        InstallType::User => &mut self.installeduserpkgs,
                        InstallType::System => &mut self.installedsystempkgs,
                    };
                    match work.action {
                        PkgAction::Install => {
                            installed.insert(name);
                            if work.pkg == self.pkg && self.launchable.is_none() && in_path(&self.pname) {
                                self.set_launchable(Some(Launch::TerminalApp(self.pname.to_string())));
                            }
                        }
                        PkgAction::Remove => {
                            installed.remove(&name);
                        }
                    }
                }
                if work.notify.is_some() {
                    sender.output(AppMsg::RemoveInstalledBusy(work));
                }
            }
            PkgMsg::ProfileBatchDone => {
                let _ = nix_data::utils::refreshicons();
                sender.output(AppMsg::UpdateInstalledPkgs);
            }
            PkgMsg::UpdateOnline(online) => {
                self.set_online(online);
            }
        }
    }

    fn update_cmd(&mut self, msg: Self::CommandOutput, sender: ComponentSender<Self>, _root: &Self::Root) {
        match msg {
            PkgAsyncMsg::LoadScreenshot(pkg, i, u) => {
                sender.input(PkgMsg::LoadScreenshot(pkg, i, u));
            }
            PkgAsyncMsg::SetError(pkg, i) => {
                sender.input(PkgMsg::SetError(pkg, i));
            }
        }
    }
}

relm4::new_action_group!(ModeActionGroup, "mode");
relm4::new_stateless_action!(NixEnvAction, ModeActionGroup, "env");
relm4::new_stateless_action!(NixProfileAction, ModeActionGroup, "profile");
relm4::new_stateless_action!(NixSystemAction, ModeActionGroup, "system");

relm4::new_action_group!(RunActionGroup, "run");
relm4::new_stateless_action!(LaunchAction, RunActionGroup, "launch");
relm4::new_stateless_action!(TermShellAction, RunActionGroup, "term");

impl PkgModel {
    fn ctx(&self) -> userops::Ctx {
        userops::Ctx {
            config: self.config.clone(),
            syspkgs: self.syspkgtype.clone(),
            userpkgs: self.userpkgtype.clone(),
        }
    }

    /// Everything goes to the app-wide queue; a removal asks first.
    fn queue_work(&mut self, w: WorkPkg, sender: &ComponentSender<Self>) {
        let ctx = self.ctx();
        if w.action == PkgAction::Remove {
            confirm_remove(w, ctx, sender.output_sender().clone());
        } else if w.action == PkgAction::Install && sourceinfo::is_unfree(&w.pkg) {
            // The licence is known for the page's own package only.
            let licence = if w.pkg == self.pkg { licence_text(&self.licenses) } else { String::new() };
            let (out, out_declined) = (sender.output_sender().clone(), sender.output_sender().clone());
            let (accepted, declined) = (w.clone(), w.clone());
            confirm_unfree(
                w.display_name().to_string(),
                licence,
                false,
                move || enqueue_work(accepted.clone(), ctx.clone(), &out),
                move || {
                    if declined.notify.is_some() {
                        out_declined.emit(AppMsg::RemoveInstalledBusy(declined.clone()));
                    }
                },
            );
        } else {
            enqueue_work(w, ctx, sender.output_sender());
        }
    }

    /// Runs the page's package without installing it; unfree software asks every time, since
    /// nothing records the acceptance.
    fn queue_run(&mut self, job: RunJob) {
        let ctx = self.ctx();
        if sourceinfo::is_unfree(&job.pkg) {
            let name = job.display_name().to_string();
            confirm_unfree(name, licence_text(&self.licenses), true, move || {
                userops::enqueue_run(job.clone(), ctx.clone());
            }, || {});
        } else {
            userops::enqueue_run(job, ctx);
        }
    }
}

/// Queues the work; clears the installed page's busy mark when it adds nothing to wait for.
fn enqueue_work(w: WorkPkg, ctx: userops::Ctx, out: &relm4::Sender<AppMsg>) {
    if !userops::enqueue(w.clone(), ctx) && w.notify.is_some() {
        out.emit(AppMsg::RemoveInstalledBusy(w));
    }
}

fn licence_text(licenses: &[License]) -> String {
    licenses
        .iter()
        .map(|l| match &l.spdxid {
            Some(id) if !id.is_empty() && id != &l.fullname => format!("{} ({})", l.fullname, id),
            _ => l.fullname.clone(),
        })
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The consent dialog for unfree software. Cancel is the default; the app and licence are plain text.
pub fn unfree_dialog(name: &str, licence: &str) -> adw::MessageDialog {
    unfree_dialog_for(name, licence, false)
}

fn unfree_dialog_for(name: &str, licence: &str, run: bool) -> adw::MessageDialog {
    let (verb, ing) = if run { ("Run", "Running") } else { ("Install", "Installing") };
    let licence = if licence.is_empty() { "not free (see the app's page)" } else { licence };
    let dialog = adw::MessageDialog::new(
        relm4::main_application().active_window().as_ref(),
        Some(&format!("{verb} unfree software?")),
        Some(&format!(
            "{name} is not free software.
Licence: {licence}

{ing} it means you accept its licence terms."
        )),
    );
    dialog.set_heading_use_markup(false);
    dialog.set_body_use_markup(false);
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("accept", verb);
    dialog.set_response_appearance("accept", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog
}

/// Asks at the Install or Run click, never when a queued item starts. An installed element is the
/// acceptance, so updates and restores do not ask again; a run leaves none.
fn confirm_unfree(name: String, licence: String, run: bool, accept: impl Fn() + 'static, decline: impl Fn() + 'static) {
    let dialog = unfree_dialog_for(&name, &licence, run);
    dialog.connect_response(None, move |_, response| {
        if response == "accept" {
            accept();
        } else {
            decline();
        }
    });
    dialog.present();
}

/// Asks before a removal; Cancel is the default and enqueues nothing.
fn confirm_remove(w: WorkPkg, ctx: userops::Ctx, out: relm4::Sender<AppMsg>) {
    let name = w.display_name().to_string();
    let from = match (&w.pkgtype, &ctx.userpkgs) {
        (InstallType::System, _) => "your system configuration",
        (InstallType::User, UserPkgs::Env) => "your user environment",
        (InstallType::User, UserPkgs::Profile) => "your profile",
    };
    let dialog = adw::MessageDialog::new(
        relm4::main_application().active_window().as_ref(),
        Some(&format!("Remove {name}?")),
        Some(&format!("{name} will be removed from {from}.")),
    );
    dialog.set_heading_use_markup(false);
    dialog.set_body_use_markup(false);
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("remove", "Remove");
    dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.connect_response(None, move |_, response| {
        if response == "remove" {
            enqueue_work(w.clone(), ctx.clone(), &out);
        } else if w.notify.is_some() {
            out.emit(AppMsg::RemoveInstalledBusy(w.clone()));
        }
    });
    dialog.present();
}
