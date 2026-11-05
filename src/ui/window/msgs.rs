use super::*;

#[derive(Debug)]
pub enum AppMsg {
    UpdateSysconfig(Option<String>),
    UpdateFlake(Option<String>, Option<String>),
    TryLoad,
    UpdateDB,
    LoadConfig(NixDataConfig),
    Close,
    LoadError(String, String),
    Initialize(
        String,
        Option<String>,
        Option<String>,
        HashMap<String, AppData>,
        Vec<String>,
        HashMap<PkgCategory, Vec<String>>,
        HashMap<PkgCategory, Vec<String>>,
    ),
    OpenPkg(String),
    FrontPage,
    FrontFrontPage,
    // UpdatePkgs(Option<Vec<String>>),
    UpdateInstalledPkgs,
    UpdateInstalledPage,
    // UpdateUpdatePkgs,
    UpdateCategoryPkgs,
    SetSearch(bool),
    SetVsBar(bool),
    SetVsChild(String),
    Search(String),
    AddInstalledToWorkQueue(WorkPkg),
    RemoveInstalledBusy(WorkPkg),
    OpenCategoryPage(PkgCategory),
    LoadCategory(PkgCategory),
    UpdateRecPkgs(Vec<String>),
    GetUnavailableItems(HashMap<String, String>, HashMap<String, String>, UpdateType),
    CheckNetwork,
    /// Work was queued, finished or changed: refresh the Activity badge.
    ActivityChanged,
    ShowTab(String),
    /// An update was queued: toast with a link to the Activity tab.
    UpdateQueued(String),
    /// The window was closed while work is queued or running.
    ConfirmClose,
    StopAndQuit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PkgItem {
    pub(super) pkg: String,
    pub(super) pname: String,
    pub(super) name: String,
    pub(super) version: String,
    pub(super) summary: Option<String>,
    pub(super) icon: Option<String>,
}

#[derive(Debug)]
pub enum AppAsyncMsg {
    Search(String, Vec<SearchItem>),
    UpdateRecPkgs(Vec<PkgTile>),
    UpdateInstalledPkgs(HashSet<String>, HashMap<String, String>),
    LoadCategory(PkgCategory, Vec<CategoryTile>, Vec<CategoryTile>),
    SetNetwork(bool),
}
