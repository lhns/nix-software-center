use crate::{
    config,
    parse::{
        config::{editconfig, getconfig},
        packages::{AppData, LicenseEnum, PkgMaintainer, Platform},
        util,
    },
    ui::{
        installedpage::InstalledItem, pkgpage::PkgPageInit,
        unavailabledialog::UnavailableDialogMsg, updatepage::UNAVAILABLE_BROKER,
        welcome::WelcomeMsg,
    },
    APPINFO,
};
use adw::prelude::*;
use log::*;
use nix_data::config::configfile::NixDataConfig;
use relm4::{
    self,
    actions::{RelmAction, RelmActionGroup},
    factory::FactoryVecDeque,
    Component, ComponentController, ComponentParts, ComponentSender, Controller, MessageBroker,
    RelmWidgetExt, WorkerController,
};
use spdx::Expression;
use sqlx::{QueryBuilder, Sqlite, SqlitePool};
use std::{
    collections::{HashMap, HashSet},
    convert::identity,
    fs,
    path::Path,
};

mod category;
mod installed;
mod msgs;
mod openpkg;
mod refresh;
mod search;
mod unavailable;

pub use msgs::{AppAsyncMsg, AppMsg, PkgItem};
use refresh::refresh_stale;

use super::{
    about::{AboutPageModel, AboutPageMsg},
    categories::{PkgCategory, PkgGroup},
    categorypage::{CategoryPageModel, CategoryPageMsg},
    categorytile::CategoryTile,
    installedpage::{InstalledPageModel, InstalledPageMsg},
    pkgpage::{self, InstallType, PkgInitModel, PkgModel, PkgMsg, WorkPkg},
    pkgtile::PkgTile,
    preferencespage::{PreferencesPageModel, PreferencesPageMsg},
    activitylog::{ActivityLogModel, LogMsg},
    activitypage::{ActivityMsg, ActivityPageModel},
    searchpage::{SearchItem, SearchPageModel, SearchPageMsg},
    unavailabledialog::UnavailableItemModel,
    userops,
    updatepage::{UpdateItem, UpdatePageInit, UpdatePageModel, UpdatePageMsg, UpdateType},
    welcome::WelcomeModel,
    windowloading::{LoadErrorModel, LoadErrorMsg, WindowAsyncHandler, WindowAsyncHandlerMsg},
};

pub static PKGPAGE_BROKER: MessageBroker<PkgMsg> = MessageBroker::new();
pub static ACTIVITY_BROKER: MessageBroker<ActivityMsg> = MessageBroker::new();
pub static ACTIVITYLOG_BROKER: MessageBroker<LogMsg> = MessageBroker::new();

#[derive(PartialEq)]
enum Page {
    FrontPage,
    PkgPage,
}

#[derive(PartialEq)]
enum MainPage {
    FrontPage,
    CategoryPage,
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum SystemPkgs {
    Legacy,
    Flake,
    None,
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum UserPkgs {
    Env,
    Profile,
}

#[tracker::track]
pub struct AppModel {
    mainwindow: adw::ApplicationWindow,
    config: NixDataConfig,
    #[tracker::no_eq]
    windowloading: WorkerController<WindowAsyncHandler>,
    #[tracker::no_eq]
    loaderrordialog: Controller<LoadErrorModel>,
    busy: bool,
    page: Page,
    mainpage: MainPage,
    // #[tracker::no_eq]
    // pkgs: HashMap<String, Package>,
    // syspkgs: HashMap<String, String>,
    // profilepkgs: Option<HashMap<String, String>>,
    // pkgitems: HashMap<String, PkgItem>,
    #[tracker::no_eq]
    pkgdb: String,
    #[tracker::no_eq]
    nixpkgsdb: Option<String>,
    #[tracker::no_eq]
    systemdb: Option<String>,
    appdata: HashMap<String, AppData>,
    installeduserpkgs: HashMap<String, String>,
    installedsystempkgs: HashSet<String>,
    syspkgtype: SystemPkgs,
    userpkgtype: UserPkgs,
    categoryrec: HashMap<PkgCategory, Vec<String>>,
    categoryall: HashMap<PkgCategory, Vec<String>>,
    #[tracker::no_eq]
    recommendedapps: FactoryVecDeque<PkgTile>,
    #[tracker::no_eq]
    categories: FactoryVecDeque<PkgGroup>,
    #[tracker::no_eq]
    pkgpage: Controller<PkgModel>,
    #[tracker::no_eq]
    searchpage: Controller<SearchPageModel>,
    #[tracker::no_eq]
    categorypage: Controller<CategoryPageModel>,
    searching: bool,
    searchquery: String,
    vschild: String,
    showvsbar: bool,
    #[tracker::no_eq]
    aboutpage: Controller<AboutPageModel>,
    #[tracker::no_eq]
    preferencespage: Controller<PreferencesPageModel>,
    #[tracker::no_eq]
    installedpage: Controller<InstalledPageModel>,
    #[tracker::no_eq]
    updatepage: Controller<UpdatePageModel>,
    viewstack: adw::ViewStack,
    installedpagebusy: Vec<(String, InstallType)>,
    #[tracker::no_eq]
    activitypage: Controller<ActivityPageModel>,
    #[tracker::no_eq]
    activitylog: Controller<ActivityLogModel>,
    #[tracker::no_eq]
    toasts: adw::ToastOverlay,
    #[tracker::no_eq]
    welcomepage: Controller<WelcomeModel>,
    online: bool,
}

#[relm4::component(pub)]
impl Component for AppModel {
    type Init = ();
    type Input = AppMsg;
    type Output = ();
    type CommandOutput = AppAsyncMsg;

    view! {
        #[name(main_window)]
        adw::ApplicationWindow {
            set_default_width: 1150,
            set_default_height: 800,
            #[name(main_stack)]
            if model.busy {
                gtk::Box {
                    set_vexpand: true,
                    set_halign: gtk::Align::Fill,
                    set_valign: gtk::Align::Fill,
                    set_orientation: gtk::Orientation::Vertical,
                    adw::HeaderBar {
                        add_css_class: "flat",
                        #[wrap(Some)]
                        set_title_widget = &gtk::Label {
                            set_label: "Nix Software Center"
                        }
                    },
                    gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_halign: gtk::Align::Center,
                        set_valign: gtk::Align::Center,
                        set_hexpand: true,
                        set_vexpand: true,
                        set_spacing: 15,
                        gtk::Image {
                            set_icon_name: Some(config::APP_ID),
                            set_pixel_size: 192,
                        },
                        gtk::Label {
                            add_css_class: "title-1",
                            set_label: "Loading...",
                        },
                    }
                }
            } else {
                gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                #[name(main_leaf)]
                adw::Leaflet {
                    set_vexpand: true,
                    set_can_unfold: false,
                    set_homogeneous: false,
                    set_transition_type: adw::LeafletTransitionType::Over,
                    set_can_navigate_back: true,
                    #[name(front_leaf)]
                    append = &adw::Leaflet {
                        set_can_unfold: false,
                        set_homogeneous: false,
                        set_transition_type: adw::LeafletTransitionType::Over,
                        set_can_navigate_back: true,
                        #[name(main_box)]
                        append = &gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            adw::HeaderBar {
                                set_centering_policy: adw::CenteringPolicy::Strict,
                                pack_start: searchbtn = &gtk::ToggleButton {
                                    add_css_class: "flat",
                                    set_icon_name: "system-search-symbolic",
                                    #[watch]
                                    #[block_signal(searchtoggle)]
                                    set_active: model.searching,
                                    connect_toggled[sender] => move |x| {
                                        sender.input(AppMsg::SetSearch(x.is_active()))
                                    } @searchtoggle

                                },
                                #[name(viewswitchertitle)]
                                #[wrap(Some)]
                                set_title_widget = &adw::ViewSwitcherTitle {
                                    set_title: "Nix Software Center",
                                    set_stack: Some(viewstack),
                                    connect_title_visible_notify[sender] => move |x| {
                                        sender.input(AppMsg::SetVsBar(x.is_title_visible()))
                                    },
                                },
                                pack_end: menu = &gtk::MenuButton {
                                    add_css_class: "flat",
                                    set_icon_name: "open-menu-symbolic",
                                    #[wrap(Some)]
                                    set_popover = &gtk::PopoverMenu::from_model(Some(&mainmenu)) {
                                        add_css_class: "menu"
                                    }
                                }
                            },
                            gtk::SearchBar {
                                #[watch]
                                set_search_mode: model.searching,
                                #[wrap(Some)]
                                set_child = &adw::Clamp {
                                    set_hexpand: true,
                                    gtk::SearchEntry {
                                        #[track(model.changed(AppModel::searching()) && model.searching)]
                                        grab_focus: (),
                                        #[track(model.changed(AppModel::searching()) && !model.searching)]
                                        set_text: "",
                                        connect_search_changed[sender] => move |x| {
                                            if x.text().len() > 1 {
                                                sender.input(AppMsg::Search(x.text().to_string()))
                                            }
                                        }
                                    }
                                }
                            },
                            #[local_ref]
                            viewstack -> adw::ViewStack {
                                connect_visible_child_notify[sender] => move |x| {
                                    if let Some(c) = x.visible_child_name() {
                                        sender.input(AppMsg::SetVsChild(c.to_string()))
                                    }
                                },
                                #[name(frontpage)]
                                add = &gtk::ScrolledWindow {
                                    set_vexpand: true,
                                    set_hexpand: true,
                                    set_hscrollbar_policy: gtk::PolicyType::Never,
                                    adw::Clamp {
                                        set_maximum_size: 1000,
                                        set_tightening_threshold: 750,
                                        gtk::Box {
                                            set_orientation: gtk::Orientation::Vertical,
                                            set_valign: gtk::Align::Start,
                                            set_margin_all: 15,
                                            set_spacing: 15,
                                            gtk::Label {
                                                set_halign: gtk::Align::Start,
                                                add_css_class: "title-4",
                                                set_label: "Categories",
                                            },
                                            #[local_ref]
                                            categorybox -> gtk::FlowBox {
                                                set_halign: gtk::Align::Fill,
                                                set_hexpand: true,
                                                set_valign: gtk::Align::Center,
                                                set_orientation: gtk::Orientation::Horizontal,
                                                set_selection_mode: gtk::SelectionMode::None,
                                                set_homogeneous: true,
                                                set_max_children_per_line: 3,
                                                set_min_children_per_line: 1,
                                                set_column_spacing: 14,
                                                set_row_spacing: 14,
                                            },
                                            gtk::Label {
                                                set_halign: gtk::Align::Start,
                                                add_css_class: "title-4",
                                                set_label: "Recommended",
                                            },
                                            #[local_ref]
                                            recbox -> gtk::FlowBox {
                                                set_halign: gtk::Align::Fill,
                                                set_hexpand: true,
                                                set_valign: gtk::Align::Center,
                                                set_orientation: gtk::Orientation::Horizontal,
                                                set_selection_mode: gtk::SelectionMode::None,
                                                set_homogeneous: true,
                                                set_max_children_per_line: 3,
                                                set_min_children_per_line: 1,
                                                set_column_spacing: 14,
                                                set_row_spacing: 14,
                                            }
                                        }
                                    }
                                },
                                add: model.installedpage.widget(),
                                add: model.searchpage.widget(),
                                add: model.updatepage.widget(),
                                add: model.activitypage.widget(),
                            },
                            adw::ViewSwitcherBar {
                                set_stack: Some(viewstack),
                                #[track(model.changed(AppModel::showvsbar()))]
                                set_reveal: model.showvsbar,
                            }
                        },
                        append: model.categorypage.widget(),
                    },
                    append: model.pkgpage.widget()
                },
                }
            }
        }
    }

    menu! {
        mainmenu: {
            "Preferences" => PreferencesAction,
            "About" => AboutAction,
        }
    }

    fn pre_view() {
        match model.page {
            Page::FrontPage => {
                main_leaf.set_visible_child(front_leaf);
            }
            Page::PkgPage => {
                main_leaf.set_visible_child(model.pkgpage.widget());
            }
        }
        match model.mainpage {
            MainPage::FrontPage => {
                front_leaf.set_visible_child(main_box);
            }
            MainPage::CategoryPage => {
                front_leaf.set_visible_child(model.categorypage.widget());
            }
        }
    }

    #[tokio::main]
    async fn init(
        _application: Self::Init,
        root: &Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let (config, welcome) = if let Some(config) = getconfig() {
            debug!("Got config: {:?}", config);
            let mut out = false;
            if let Some(configpath) = &config.systemconfig {
                if !Path::new(configpath).exists() {
                    warn!("Invalid system config path: {}", configpath);
                    out = true
                }
            }
            if let Some(flakepath) = &config.flake {
                if !Path::new(&flakepath).exists() {
                    warn!("Invalid flake path: {}", flakepath);
                    out = true
                }
            }
            (config, out)
        } else {
            // Show welcome page
            debug!("No config found");
            (
                NixDataConfig {
                    systemconfig: None,
                    flake: None,
                    flakearg: None,
                    generations: None,
                },
                true,
            )
        };

        let userpkgtype = if let Ok(h) = std::env::var("HOME") {
            if Path::new(&format!("{}/.nix-profile/manifest.json", h)).exists()
                || !Path::new("/nix/var/nix/profiles/per-user/root/channels/nixos").exists()
                || !Path::new(&format!("{}/.nix-profile/manifest.nix", h)).exists()
                || if let Ok(m) = fs::read_to_string(&format!("{}/.nix-profile/manifest.nix", h)) {
                    m == "[ ]"
                } else {
                    false
                }
            {
                UserPkgs::Profile
            } else {
                UserPkgs::Env
            }
        } else {
            UserPkgs::Env
        };
        let nixos = nsc_backend::sources::is_nixos();
        let syspkgtype = if config.systemconfig.is_none() || !nixos {
            SystemPkgs::None
        } else {
            match fs::read_to_string("/run/current-system/nixos-version") {
                Ok(s) => {
                    if !Path::new("/nix/var/nix/profiles/per-user/root/channels/nixos").exists()
                        || config.flake.is_some()
                    {
                        SystemPkgs::Flake
                    } else if let Some(last) = s.split('.').last() {
                        if last.len() == 7 || last == "dirty" || last == "git" {
                            SystemPkgs::Flake
                        } else {
                            SystemPkgs::Legacy
                        }
                    } else {
                        SystemPkgs::Legacy
                    }
                }
                Err(_) => SystemPkgs::None,
            }
        };

        debug!("userpkgtype: {:?}", userpkgtype);
        debug!("syspkgtype: {:?}", syspkgtype);

        let online = util::checkonline();

        let windowloading = WindowAsyncHandler::builder()
            .detach_worker(())
            .forward(sender.input_sender(), identity);
        let loaderrordialog = LoadErrorModel::builder()
            .launch(root.clone().upcast())
            .forward(sender.input_sender(), identity);
        let pkgpage = PkgModel::builder()
            .launch_with_broker(PkgPageInit {
                userpkgs: userpkgtype.clone(),
                syspkgs: syspkgtype.clone(),
                config: config.clone(),
                online,
            }, &PKGPAGE_BROKER)
            .forward(sender.input_sender(), identity);
        let searchpage = SearchPageModel::builder()
            .launch(())
            .forward(sender.input_sender(), identity);
        let categorypage = CategoryPageModel::builder()
            .launch(())
            .forward(sender.input_sender(), identity);
        let installedpage = InstalledPageModel::builder()
            .launch((syspkgtype.clone(), userpkgtype.clone()))
            .forward(sender.input_sender(), identity);
        let updatepage = UpdatePageModel::builder()
            .launch(UpdatePageInit {
                window: root.clone().upcast(),
                systype: syspkgtype.clone(),
                usertype: userpkgtype.clone(),
                config: config.clone(),
                online,
            })
            .forward(sender.input_sender(), identity);
        let activitypage = ActivityPageModel::builder()
            .launch_with_broker((), &ACTIVITY_BROKER)
            .forward(sender.input_sender(), identity);
        let activitylog = ActivityLogModel::builder()
            .launch_with_broker((), &ACTIVITYLOG_BROKER)
            .detach();
        let viewstack = adw::ViewStack::new();
        let welcomepage = WelcomeModel::builder()
            .launch(root.clone().upcast())
            .forward(sender.input_sender(), identity);
        let aboutpage = AboutPageModel::builder()
            .launch(root.clone().upcast())
            .detach();
        let preferencespage = PreferencesPageModel::builder()
            .launch(root.clone().upcast())
            .forward(sender.input_sender(), identity);

        let model = AppModel {
            mainwindow: root.clone(),
            config,
            windowloading,
            loaderrordialog,
            busy: true,
            page: Page::FrontPage,
            mainpage: MainPage::FrontPage,
            pkgdb: String::new(),
            nixpkgsdb: None,
            systemdb: None,
            appdata: HashMap::new(),
            installeduserpkgs: HashMap::new(),
            installedsystempkgs: HashSet::new(),
            syspkgtype,
            userpkgtype,
            categoryrec: HashMap::new(),
            categoryall: HashMap::new(),
            recommendedapps: FactoryVecDeque::new(gtk::FlowBox::new(), sender.input_sender()),
            categories: FactoryVecDeque::new(gtk::FlowBox::new(), sender.input_sender()),
            pkgpage,
            searchpage,
            categorypage,
            searching: false,
            searchquery: String::default(),
            vschild: String::default(),
            showvsbar: false,
            installedpage,
            updatepage,
            viewstack,
            installedpagebusy: vec![],
            activitypage,
            activitylog,
            toasts: adw::ToastOverlay::new(),
            welcomepage,
            aboutpage,
            preferencespage,
            online,
            tracker: 0,
        };

        if welcome && nixos {
            model.welcomepage.emit(WelcomeMsg::Show);
        } else {
            model.windowloading.emit(WindowAsyncHandlerMsg::CheckCache(
                model.syspkgtype.clone(),
                model.userpkgtype.clone(),
                model.config.clone(),
            ));
        }
        let recbox = model.recommendedapps.widget();
        let categorybox = model.categories.widget();
        let viewstack = &model.viewstack;

        let widgets = view_output!();

        let mut group = RelmActionGroup::<MenuActionGroup>::new();
        let aboutpage: RelmAction<AboutAction> = {
            let sender = model.aboutpage.sender().clone();
            RelmAction::new_stateless(move |_| {
                sender.send(()).unwrap();
            })
        };

        let prefernecespage: RelmAction<PreferencesAction> = {
            let sender = model.preferencespage.sender().clone();
            let preferencespage = model.preferencespage.widget().clone();
            let config = model.config.clone();
            RelmAction::new_stateless(move |_| {
                sender.send(PreferencesPageMsg::Show(config.clone())).unwrap();
                preferencespage.present();
            })
        };

        group.add_action(aboutpage);
        group.add_action(prefernecespage);
        let actions = group.into_action_group();
        widgets
            .main_window
            .insert_action_group("menu", Some(&actions));

        if !nsc_backend::sources::is_nixos() {
            // Nothing to configure but the sources.
            if let Some(menu) = widgets
                .menu
                .popover()
                .and_then(|p| p.downcast::<gtk::PopoverMenu>().ok())
                .and_then(|p| p.menu_model())
                .and_then(|m| m.downcast::<gtk::gio::Menu>().ok())
            {
                menu.remove(0);
                menu.insert(0, Some("Sources"), Some("menu.preferences"));
            }
        }

        widgets.main_stack.set_vhomogeneous(false);
        widgets.main_stack.set_hhomogeneous(false);
        let frontvs = widgets.viewstack.page(&widgets.frontpage);
        let installedvs = widgets.viewstack.page(model.installedpage.widget());
        let updatesvs = widgets.viewstack.page(model.updatepage.widget());
        let searchvs = widgets.viewstack.page(model.searchpage.widget());
        frontvs.set_title(Some("Explore"));
        installedvs.set_title(Some("Installed"));
        updatesvs.set_title(Some("Updates"));
        frontvs.set_name(Some("explore"));
        installedvs.set_name(Some("installed"));
        searchvs.set_name(Some("search"));
        updatesvs.set_name(Some("updates"));
        frontvs.set_icon_name(Some("nsc-home-symbolic"));
        installedvs.set_icon_name(Some("nsc-installed-symbolic"));
        updatesvs.set_icon_name(Some("nsc-update-symbolic"));

        let activityvs = widgets.viewstack.page(model.activitypage.widget());
        activityvs.set_title(Some("Activity"));
        activityvs.set_name(Some("activity"));
        activityvs.set_icon_name(Some("view-list-symbolic"));

        // The log modal covers the whole window, package pages included, so it
        // wraps the main stack rather than sitting in a tab.
        let stack = widgets.main_stack.clone();
        widgets.main_window.set_content(None::<&gtk::Widget>);
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&stack));
        overlay.add_overlay(model.activitylog.widget());
        model.toasts.set_child(Some(&overlay));
        widgets.main_window.set_content(Some(&model.toasts));
        super::activitylog::hide_on_escape(&widgets.main_window);
        widgets.main_window.connect_close_request({
            let sender = sender.clone();
            move |_| {
                if userops::is_busy() {
                    sender.input(AppMsg::ConfirmClose);
                    gtk::Inhibit(true)
                } else {
                    gtk::Inhibit(false)
                }
            }
        });

        ComponentParts { model, widgets }
    }

    #[tokio::main]
    async fn update(
        &mut self,
        msg: Self::Input,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        self.reset();
        match msg {
            AppMsg::TryLoad => {
                self.busy = true;
                self.windowloading.emit(WindowAsyncHandlerMsg::CheckCache(
                    self.syspkgtype.clone(),
                    self.userpkgtype.clone(),
                    self.config.clone(),
                ));
            }
            AppMsg::UpdateDB => {
                self.windowloading.emit(WindowAsyncHandlerMsg::UpdateDB(
                    self.syspkgtype.clone(),
                    self.userpkgtype.clone(),
                ));
            }
            AppMsg::LoadConfig(config) => {
                info!("AppMsg::LoadConfig");
                self.config = config;
                if let Err(e) = editconfig(self.config.clone()) {
                    warn!("Error editing config: {}", e);
                }
                let nixos = nsc_backend::sources::is_nixos();
                self.syspkgtype = if self.config.systemconfig.is_none() || !nixos {
                    SystemPkgs::None
                } else {
                    match fs::read_to_string("/run/current-system/nixos-version") {
                        Ok(s) => {
                            if !Path::new("/nix/var/nix/profiles/per-user/root/channels/nixos")
                                .exists()
                                || self.config.flake.is_some()
                            {
                                SystemPkgs::Flake
                            } else if let Some(last) = s.split('.').last() {
                                if last.len() == 7 || last == "dirty" || last == "git" {
                                    SystemPkgs::Flake
                                } else {
                                    SystemPkgs::Legacy
                                }
                            } else {
                                SystemPkgs::Legacy
                            }
                        }
                        Err(_) => SystemPkgs::None,
                    }
                };
                self.pkgpage.emit(PkgMsg::UpdatePkgTypes(
                    self.syspkgtype.clone(),
                    self.userpkgtype.clone(),
                ));
                self.pkgpage.emit(PkgMsg::UpdateConfig(self.config.clone()));
                self.updatepage.emit(UpdatePageMsg::UpdatePkgTypes(
                    self.syspkgtype.clone(),
                    self.userpkgtype.clone(),
                ));
                self.updatepage
                    .emit(UpdatePageMsg::UpdateConfig(self.config.clone()));
                self.windowloading.emit(WindowAsyncHandlerMsg::CheckCache(
                    self.syspkgtype.clone(),
                    self.userpkgtype.clone(),
                    self.config.clone(),
                ));
            }
            AppMsg::Close => {
                relm4::main_application().quit();
            }
            AppMsg::LoadError(msg, msg2) => {
                self.busy = false;
                self.loaderrordialog.emit(LoadErrorMsg::Show(msg, msg2));
            }
            AppMsg::UpdateSysconfig(systemconfig) => {
                self.config = NixDataConfig {
                    systemconfig: systemconfig.clone(),
                    flake: self.config.flake.clone(),
                    flakearg: self.config.flakearg.clone(),
                    generations: self.config.generations,
                };
                if editconfig(self.config.clone()).is_err() {
                    warn!("Failed to update config");
                }
                let nixos = nsc_backend::sources::is_nixos();
                if systemconfig.is_some() && nixos {
                    if self.syspkgtype == SystemPkgs::None {
                        if self.config.flake.is_some() {
                            self.syspkgtype = SystemPkgs::Flake;
                        } else {
                            self.syspkgtype = SystemPkgs::Legacy;
                        }
                    }
                } else {
                    self.syspkgtype = SystemPkgs::None;
                }

                self.pkgpage.emit(PkgMsg::UpdateConfig(self.config.clone()));
                self.updatepage
                    .emit(UpdatePageMsg::UpdateConfig(self.config.clone()));
                self.pkgpage.emit(PkgMsg::UpdatePkgTypes(
                    self.syspkgtype.clone(),
                    self.userpkgtype.clone(),
                ));
                self.updatepage.emit(UpdatePageMsg::UpdatePkgTypes(
                    self.syspkgtype.clone(),
                    self.userpkgtype.clone(),
                ));
                self.installedpage.emit(InstalledPageMsg::UpdatePkgTypes(
                    self.syspkgtype.clone(),
                    self.userpkgtype.clone(),
                ));
            }
            AppMsg::UpdateFlake(flake, flakearg) => {
                self.config = NixDataConfig {
                    systemconfig: self.config.systemconfig.clone(),
                    flake: flake.clone(),
                    flakearg,
                    generations: self.config.generations,
                };
                if editconfig(self.config.clone()).is_err() {
                    warn!("Failed to update config");
                }

                let nixos = nsc_backend::sources::is_nixos();
                if nixos {
                    if flake.is_some() {
                        self.syspkgtype = SystemPkgs::Flake;
                    } else {
                        self.syspkgtype = SystemPkgs::Legacy;
                    }
                }

                self.pkgpage.emit(PkgMsg::UpdateConfig(self.config.clone()));
                self.updatepage
                    .emit(UpdatePageMsg::UpdateConfig(self.config.clone()));
                self.pkgpage.emit(PkgMsg::UpdatePkgTypes(
                    self.syspkgtype.clone(),
                    self.userpkgtype.clone(),
                ));
                self.updatepage.emit(UpdatePageMsg::UpdatePkgTypes(
                    self.syspkgtype.clone(),
                    self.userpkgtype.clone(),
                ));
                self.installedpage.emit(InstalledPageMsg::UpdatePkgTypes(
                    self.syspkgtype.clone(),
                    self.userpkgtype.clone(),
                ));
            }
            AppMsg::Initialize(
                pkgdb,
                nixpkgsdb,
                systemdb,
                appdata,
                recommendedapps,
                categoryrec,
                categoryall,
            ) => {
                info!("AppMsg::Initialize");
                self.pkgdb = pkgdb;
                self.nixpkgsdb = nixpkgsdb;
                self.systemdb = systemdb;
                self.appdata = appdata;
                self.categoryrec = categoryrec;
                self.categoryall = categoryall;

                self.pkgpage.emit(PkgMsg::UpdateConfig(self.config.clone()));
                self.updatepage
                    .emit(UpdatePageMsg::UpdateConfig(self.config.clone()));
                self.updatepage.emit(UpdatePageMsg::SetPkgDb(
                    self.nixpkgsdb.clone().unwrap_or_else(|| self.pkgdb.clone()),
                ));
                sender.input(AppMsg::UpdateRecPkgs(recommendedapps));
                let mut cat_guard = self.categories.guard();
                cat_guard.clear();
                for c in vec![
                    PkgCategory::Audio,
                    PkgCategory::Development,
                    PkgCategory::Games,
                    PkgCategory::Graphics,
                    PkgCategory::Web,
                    PkgCategory::Video,
                ] {
                    cat_guard.push_back(c);
                }
                cat_guard.drop();
                self.busy = false;
                #[cfg(feature = "uitest")]
                let ui_test = std::env::var("NSC_UI_TEST").ok();
                #[cfg(not(feature = "uitest"))]
                let ui_test: Option<String> = None;
                if ui_test.is_none() {
                    refresh_stale(sender.input_sender().clone());
                }
                #[cfg(feature = "uitest")]
                if let Some(dir) = ui_test {
                    super::uitest::start(dir, root.clone().upcast(), sender.input_sender().clone());
                }
            }
            AppMsg::UpdateRecPkgs(pkgs) => self.update_rec_pkgs(&sender, pkgs).await,
            AppMsg::OpenPkg(pkg) => self.open_pkg(&sender, pkg).await,
            AppMsg::FrontPage => {
                self.page = Page::FrontPage;
            }
            AppMsg::FrontFrontPage => {
                self.page = Page::FrontPage;
                self.mainpage = MainPage::FrontPage;
            }
            AppMsg::UpdateInstalledPkgs => self.update_installed_pkgs(&sender).await,
            AppMsg::UpdateInstalledPage => self.update_installed_page(&sender).await,
            AppMsg::UpdateCategoryPkgs => {
                self.categorypage.emit(CategoryPageMsg::UpdateInstalled(
                    self.installeduserpkgs.keys().cloned().collect::<Vec<_>>(),
                    self.installedsystempkgs.iter().cloned().collect::<Vec<_>>(),
                ));
            }
            AppMsg::SetSearch(show) => {
                self.set_searching(show);
                if !show {
                    if let Some(s) = self.viewstack.visible_child_name() {
                        if s == "search" {
                            self.viewstack.set_visible_child_name("explore");
                        }
                    }
                }
            }
            AppMsg::SetVsChild(name) => {
                if name != self.vschild {
                    self.set_vschild(name.to_string());
                    if name != "search" {
                        sender.input(AppMsg::SetSearch(false))
                    }
                }
                if name == "updates" && self.online {
                    sender.input(AppMsg::CheckNetwork);
                }
                if name == "activity" {
                    sender.input(AppMsg::ActivityChanged);
                }
            }
            AppMsg::ActivityChanged => {
                let visible = self.viewstack.visible_child_name().map_or(false, |n| n == "activity");
                if visible {
                    userops::mark_failures_seen();
                }
                let (active, unseen) = userops::counts();
                let page = self.viewstack.page(self.activitypage.widget());
                page.set_badge_number(active as u32);
                page.set_needs_attention(unseen > 0);
            }
            AppMsg::ShowTab(name) => {
                self.viewstack.set_visible_child_name(&name);
            }
            AppMsg::UpdateQueued(title) => {
                // Toast titles are markup.
                let toast = adw::Toast::new(&gtk::glib::markup_escape_text(&title));
                toast.set_button_label(Some("View"));
                let input = sender.input_sender().clone();
                toast.connect_button_clicked(move |_| input.emit(AppMsg::ShowTab("activity".into())));
                self.toasts.add_toast(toast);
            }
            AppMsg::ConfirmClose => {
                let dialog = adw::MessageDialog::new(
                    relm4::main_application().active_window().as_ref(),
                    Some("Quit while work is running?"),
                    Some("Installs, removals or updates are still running or waiting. Stopping asks Nix to cancel them, and Nix may keep running for a moment."),
                );
                dialog.add_response("keep", "Keep Working");
                dialog.add_response("stop", "Stop and Quit");
                dialog.set_response_appearance("stop", adw::ResponseAppearance::Destructive);
                dialog.set_default_response(Some("keep"));
                dialog.set_close_response("keep");
                let input = sender.input_sender().clone();
                dialog.connect_response(None, move |_, response| {
                    if response == "stop" {
                        input.emit(AppMsg::StopAndQuit);
                    }
                });
                dialog.present();
            }
            AppMsg::StopAndQuit => {
                userops::stop_all();
                let input = sender.input_sender().clone();
                let started = std::time::Instant::now();
                gtk::glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
                    if !userops::is_busy() || started.elapsed() > std::time::Duration::from_secs(5) {
                        input.emit(AppMsg::Close);
                        return gtk::glib::Continue(false);
                    }
                    gtk::glib::Continue(true)
                });
            }
            AppMsg::SetVsBar(vsbar) => {
                self.set_showvsbar(vsbar);
            }
            AppMsg::Search(search) => self.search(&sender, search).await,
            AppMsg::AddInstalledToWorkQueue(work) => {
                let p = match work.pkgtype {
                    InstallType::User => work.pname.to_string(),
                    InstallType::System => work.pkg.to_string(),
                };
                self.installedpagebusy.push((p, work.pkgtype.clone()));
                self.pkgpage.emit(PkgMsg::AddToQueue(work));
            }
            AppMsg::RemoveInstalledBusy(work) => {
                let p = match work.pkgtype {
                    InstallType::User => work.pname.to_string(),
                    InstallType::System => work.pkg.to_string(),
                };
                self.installedpagebusy
                    .retain(|(x, y)| x != &p || y != &work.pkgtype);
                self.installedpage.emit(InstalledPageMsg::UnsetBusy(work));
            }
            AppMsg::OpenCategoryPage(category) => {
                info!("AppMsg::OpenCategoryPage({:?})", category);
                self.page = Page::FrontPage;
                self.mainpage = MainPage::CategoryPage;
                self.categorypage
                    .emit(CategoryPageMsg::Loading(category.clone()));
                sender.input(AppMsg::LoadCategory(category));
            }
            AppMsg::LoadCategory(category) => self.load_category(&sender, category).await,
            AppMsg::GetUnavailableItems(userpkgs, syspkgs, updatetype) => self.get_unavailable_items(&sender, userpkgs, syspkgs, updatetype).await,
            AppMsg::CheckNetwork => {
                let selfonline = self.online;
                let senderclone = sender.clone();
                sender.oneshot_command(async move {
                    info!("AppMsg::CheckNetwork");
                    let online = util::checkonline();
                    if online && !selfonline {
                        senderclone.input(AppMsg::UpdateDB);
                    }
                    AppAsyncMsg::SetNetwork(online)
                });
            }
        }
    }

    fn update_cmd(
        &mut self,
        msg: Self::CommandOutput,
        sender: ComponentSender<Self>,
        _root: &Self::Root,
    ) {
        match msg {
            AppAsyncMsg::Search(search, pkgitems) => {
                if search == self.searchquery {
                    self.searchpage.emit(SearchPageMsg::Search(pkgitems))
                }
            }
            AppAsyncMsg::UpdateRecPkgs(pkgtiles) => {
                info!("AppAsyncMsg::UpdateRecPkgs");
                let mut recapps_guard = self.recommendedapps.guard();
                recapps_guard.clear();
                for tile in pkgtiles {
                    recapps_guard.push_back(tile);
                }
                recapps_guard.drop();
                sender.input(AppMsg::UpdateInstalledPkgs);
                info!("DONE AppAsyncMsg::UpdateRecPkgs");
            }
            AppAsyncMsg::UpdateInstalledPkgs(installedsystempkgs, installeduserpkgs) => {
                info!("AppAsyncMsg::UpdateInstalledPkgs");
                if installedsystempkgs != self.installedsystempkgs
                    || installeduserpkgs != self.installeduserpkgs
                {
                    warn!("Changes needed!");
                    self.installedsystempkgs = installedsystempkgs;
                    self.installeduserpkgs = installeduserpkgs;
                    sender.input(AppMsg::UpdateInstalledPage);
                    debug!("Getting recommended apps guard");
                    let mut recommendedapps_guard = self.recommendedapps.guard();
                    debug!("Got recommended apps guard");
                    for item in recommendedapps_guard.iter_mut() {
                        debug!("Got item {}", item.pkg);
                        item.installeduser =
                            self.installeduserpkgs.contains_key(match self.userpkgtype {
                                UserPkgs::Env => &item.pname,
                                UserPkgs::Profile => &item.pkg,
                            });
                        item.installedsystem = self.installedsystempkgs.contains(&item.pkg);
                    }
                    if self.searching {
                        self.searchpage.emit(SearchPageMsg::UpdateInstalled(
                            self.installeduserpkgs.keys().cloned().collect(),
                            self.installedsystempkgs.clone(),
                        ));
                    }
                }
                info!("DONE AppAsyncMsg::UpdateInstalledPkgs");
            }
            AppAsyncMsg::LoadCategory(category, catrec, catall) => {
                self.categorypage
                    .emit(CategoryPageMsg::Open(category, catrec, catall));
            }
            AppAsyncMsg::SetNetwork(online) => {
                self.online = online;
                self.updatepage.emit(UpdatePageMsg::UpdateOnline(online));
                self.pkgpage.emit(PkgMsg::UpdateOnline(online));
            }
        }
    }
}

relm4::new_action_group!(MenuActionGroup, "menu");
relm4::new_stateless_action!(AboutAction, MenuActionGroup, "about");
relm4::new_stateless_action!(PreferencesAction, MenuActionGroup, "preferences");
