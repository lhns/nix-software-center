use super::*;

impl AppModel {
    pub(super) async fn update_installed_pkgs(&mut self, sender: &ComponentSender<AppModel>) {
        info!("AppMsg::UpdateInstalledPkgs");
        let systemconfig = self.config.systemconfig.clone();
        let syspkgtype = self.syspkgtype.clone();
        let userpkgtype = self.userpkgtype.clone();
        let pkgdb = self.pkgdb.clone();
        sender.oneshot_command(async move {
            let installedsystempkgs = if let Some(config) = &systemconfig {
                match syspkgtype {
                    SystemPkgs::Flake => {
                        let pkgs = nix_data::cache::flakes::getflakepkgs(&[config]).await;
                        if let Ok(pkgs) = pkgs {
                            pkgs.keys().cloned().collect::<HashSet<String>>()
                        } else {
                            HashSet::new()
                        }
                    }
                    SystemPkgs::Legacy => {
                        let pkgs = nix_data::cache::channel::getlegacypkgs(&[config]).await;
                        if let Ok(pkgs) = pkgs {
                            pkgs.keys().cloned().collect::<HashSet<String>>()
                        } else {
                            HashSet::new()
                        }
                    }
                    _ => HashSet::new(),
                }
            } else {
                HashSet::new()
            };

            let installeduserpkgs = match userpkgtype {
                UserPkgs::Profile => match userops::installed_versions(&pkgdb).await {
                    Ok(pkgs) => pkgs,
                    Err(e) => {
                        // Nix without `profile list --json`
                        warn!("nix profile list: {}; reading manifest.json", e);
                        nix_data::cache::profile::getprofilepkgs_versioned()
                            .await
                            .unwrap_or_default()
                    }
                },
                UserPkgs::Env => {
                    let pkgs = nix_data::cache::channel::getenvpkgs();
                    if let Ok(pkgs) = pkgs {
                        pkgs
                    } else {
                        HashMap::new()
                    }
                }
            };
            AppAsyncMsg::UpdateInstalledPkgs(installedsystempkgs, installeduserpkgs)
        });
    }

    pub(super) async fn update_installed_page(&mut self, sender: &ComponentSender<AppModel>) {
        info!("AppMsg::UpdateInstalledPage");
        let mut installeduseritems = vec![];
        let mut updateuseritems = vec![];
        // let pool = SqlitePool::connect(&self.pkgdb).await.unwrap();
        debug!("Installed user pkgs: {:?}", self.installeduserpkgs);
        debug!("Installed system pkgs: {:?}", self.installedsystempkgs);
        if let Ok(pool) = &SqlitePool::connect(&format!("sqlite://{}", self.pkgdb)).await {
            match self.userpkgtype {
                UserPkgs::Env => {
                    for (installedpname, installedver) in &self.installeduserpkgs {
                        let possibleitems: Vec<(String,)> =
                            sqlx::query_as("SELECT attribute FROM pkgs WHERE pname = $1")
                                .bind(installedpname)
                                .fetch_all(pool)
                                .await
                                .unwrap();
                        match possibleitems.len() {
                            1 => {
                                let (pkg,) = &possibleitems[0];
                                let (description,): (String,) = sqlx::query_as(
                                    "SELECT description FROM meta WHERE attribute = $1",
                                )
                                .bind(pkg)
                                .fetch_one(pool)
                                .await
                                .unwrap();
                                let (newver,): (String,) = sqlx::query_as(
                                    "SELECT version FROM pkgs WHERE attribute = $1",
                                )
                                .bind(pkg)
                                .fetch_one(pool)
                                .await
                                .unwrap();
                                let mut name = installedpname.to_string();
                                let mut summary = if description.is_empty() {
                                    None
                                } else {
                                    Some(description.to_string())
                                };
                                let mut icon = None;
                                if let Some(data) = self.appdata.get(pkg) {
                                    if let Some(n) = &data.name {
                                        if let Some(n) = n.get("C") {
                                            name = n.to_string();
                                        }
                                    }
                                    if let Some(s) = &data.summary {
                                        if let Some(s) = s.get("C") {
                                            summary = Some(s.to_string());
                                        }
                                    }
                                    if let Some(i) = &data.icon {
                                        if let Some(i) = &i.cached {
                                            icon = Some(i[0].name.clone());
                                        }
                                    }
                                }
                                installeduseritems.push(InstalledItem {
                                    name: name.clone(),
                                    pname: installedpname.to_string(),
                                    pkg: Some(pkg.clone()),
                                    summary: summary.clone(),
                                    icon: icon.clone(),
                                    pkgtype: InstallType::User,
                                    busy: self.installedpagebusy.contains(&(
                                        installedpname.to_string(),
                                        InstallType::User,
                                    )),
                                });
                                if !installedver.eq(&newver) {
                                    updateuseritems.push(UpdateItem {
                                        name,
                                        pname: installedpname.to_string(),
                                        pkg: Some(pkg.clone()),
                                        summary,
                                        icon,
                                        pkgtype: InstallType::User,
                                        verfrom: Some(installedver.to_string()),
                                        verto: Some(newver),
                                    });
                                }
                            }
                            2.. => {
                                installeduseritems.push(InstalledItem {
                                    name: installedpname.clone(),
                                    pname: installedpname.clone(),
                                    pkg: None,
                                    summary: None,
                                    icon: None,
                                    pkgtype: InstallType::User,
                                    busy: self.installedpagebusy.contains(&(
                                        installedpname.clone(),
                                        InstallType::User,
                                    )),
                                });
                                let possibleversions: Vec<(String,)> = sqlx::query_as(
                                    "SELECT version FROM pkgs WHERE pname =  $1",
                                )
                                .bind(installedpname)
                                .fetch_all(pool)
                                .await
                                .unwrap();
                                if !possibleversions.contains(&(installedver.to_string(),))
                                {
                                    updateuseritems.push(UpdateItem {
                                        name: installedpname.clone(),
                                        pname: installedpname.clone(),
                                        pkg: None,
                                        summary: None,
                                        icon: None,
                                        pkgtype: InstallType::User,
                                        verfrom: Some(installedver.to_string()),
                                        verto: None,
                                    });
                                }
                            }
                            _ => {}
                        }
                    }
                }
                UserPkgs::Profile => {
                    for (installedpkg, installedver) in &self.installeduserpkgs {
                        debug!("Checking package {}", installedpkg);
                        // Entries missing from the database (other flakes,
                        // renamed attributes) are listed by their key.
                        let dbrow: Option<(String, String, Option<String>)> = sqlx::query_as(
                            "SELECT pname, version, description FROM pkgs LEFT JOIN meta ON (pkgs.attribute = meta.attribute) WHERE pkgs.attribute = $1",
                        )
                        .bind(installedpkg)
                        .fetch_optional(pool)
                        .await
                        .ok()
                        .flatten();
                        let indb = dbrow.is_some();
                        let (pname, dbversion, description) = dbrow.unwrap_or_else(|| {
                            (installedpkg.clone(), String::new(), None)
                        });
                        let version = if installedver.is_empty() {
                            dbversion
                        } else {
                            installedver.clone()
                        };
                        let mut name = pname.to_string();
                        let mut summary = description.filter(|d| !d.is_empty());
                        let mut icon = None;
                        if let Some(data) = self.appdata.get(installedpkg) {
                            if let Some(n) = &data.name {
                                if let Some(n) = n.get("C") {
                                    name = n.to_string();
                                }
                            }
                            if let Some(s) = &data.summary {
                                if let Some(s) = s.get("C") {
                                    summary = Some(s.to_string());
                                }
                            }
                            if let Some(i) = &data.icon {
                                if let Some(i) = &i.cached {
                                    icon = Some(i[0].name.clone());
                                }
                            }
                        }
                        installeduseritems.push(InstalledItem {
                            name: name.to_string(),
                            pname: pname.to_string(),
                            pkg: Some(installedpkg.clone()),
                            summary: summary.clone(),
                            icon: icon.clone(),
                            pkgtype: InstallType::User,
                            busy: self
                                .installedpagebusy
                                .contains(&(installedpkg.clone(), InstallType::User)),
                        });
                        if !indb {
                            continue;
                        }
                        if let Some(latest) = &self.nixpkgsdb {
                            if let Ok(latestpool) =
                                &SqlitePool::connect(&format!("sqlite://{}", latest)).await
                            {
                                let newver: Option<(String,)> = sqlx::query_as(
                                    "SELECT version FROM pkgs WHERE attribute = $1",
                                )
                                .bind(installedpkg)
                                .fetch_optional(latestpool)
                                .await
                                .ok()
                                .flatten();
                                let Some((newver,)) = newver else { continue };
                                debug!("PROFILE: {} {} {}", installedpkg, version, newver);
                                if version != newver {
                                    updateuseritems.push(UpdateItem {
                                        name,
                                        pname,
                                        pkg: Some(installedpkg.clone()),
                                        summary,
                                        icon,
                                        pkgtype: InstallType::System,
                                        verfrom: Some(version.clone()),
                                        verto: Some(newver.clone()),
                                    })
                                }
                            }
                        }
                    }
                }
            }

            installeduseritems
                .sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
            let mut installedsystemitems = vec![];
            let mut updatesystemitems = vec![];
            for installedpkg in &self.installedsystempkgs {
                let versionpname: sqlx::Result<(String, String)> =
                    sqlx::query_as("SELECT pname, version FROM pkgs where attribute = $1")
                        .bind(installedpkg)
                        .fetch_one(pool)
                        .await;
                if let Ok((pname, version)) = versionpname {
                    let desc: sqlx::Result<(String,)> =
                        sqlx::query_as("SELECT description FROM meta WHERE attribute = $1")
                            .bind(installedpkg)
                            .fetch_one(pool)
                            .await;
                    if let Ok((description,)) = desc {
                        let mut name = pname.to_string();
                        let mut summary = if description.is_empty() {
                            None
                        } else {
                            Some(description)
                        };
                        let mut icon = None;
                        if let Some(data) = self.appdata.get(installedpkg) {
                            if let Some(n) = &data.name {
                                if let Some(n) = n.get("C") {
                                    name = n.to_string();
                                }
                            }
                            if let Some(s) = &data.summary {
                                if let Some(s) = s.get("C") {
                                    summary = Some(s.to_string());
                                }
                            }
                            if let Some(i) = &data.icon {
                                if let Some(i) = &i.cached {
                                    icon = Some(i[0].name.clone());
                                }
                            }
                        }
                        installedsystemitems.push(InstalledItem {
                            name: name.to_string(),
                            pname: pname.to_string(),
                            pkg: Some(installedpkg.clone()),
                            summary: summary.clone(),
                            icon: icon.clone(),
                            pkgtype: InstallType::System,
                            busy: self
                                .installedpagebusy
                                .contains(&(installedpkg.clone(), InstallType::System)),
                        });
                        if let Some(current) = &self.systemdb {
                            if let Ok(currentpool) =
                                &SqlitePool::connect(&format!("sqlite://{}", current)).await
                            {
                                let currver: Option<(String,)> = sqlx::query_as(
                                    "SELECT version FROM pkgs WHERE attribute = $1",
                                )
                                .bind(installedpkg)
                                .fetch_optional(currentpool)
                                .await
                                .ok()
                                .flatten();
                                let Some((currver,)) = currver else { continue };
                                debug!("SYSTEM: {} {} {}", installedpkg, currver, version);
                                if version != currver {
                                    updatesystemitems.push(UpdateItem {
                                        name,
                                        pname,
                                        pkg: Some(installedpkg.clone()),
                                        summary,
                                        icon,
                                        pkgtype: InstallType::System,
                                        verfrom: Some(currver.clone()),
                                        verto: Some(version.clone()),
                                    })
                                }
                            }
                        }
                    }
                }
            }

            // Add NixOS system to update list
            match self.syspkgtype {
                SystemPkgs::Legacy => {
                    if let Ok(Some((old, new))) = nix_data::cache::channel::uptodate() {
                        updatesystemitems.insert(
                            0,
                            UpdateItem {
                                name: String::from("NixOS System"),
                                pname: String::new(),
                                pkg: None,
                                summary: Some(String::from(
                                    "NixOS internal packages and modules",
                                )),
                                icon: None,
                                pkgtype: InstallType::System,
                                verfrom: Some(old),
                                verto: Some(new),
                            },
                        )
                    }
                }
                SystemPkgs::Flake => {
                    if let Ok(Some((old, new))) = nix_data::cache::flakes::uptodate() {
                        updatesystemitems.insert(
                            0,
                            UpdateItem {
                                name: String::from("NixOS System"),
                                pname: String::new(),
                                pkg: None,
                                summary: Some(String::from(
                                    "NixOS internal packages and modules",
                                )),
                                icon: None,
                                pkgtype: InstallType::System,
                                verfrom: Some(old),
                                verto: Some(new),
                            },
                        )
                    }
                }
                SystemPkgs::None => {}
            }

            installedsystemitems
                .sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
            self.installedpage.emit(InstalledPageMsg::Update(
                installeduseritems,
                installedsystemitems,
            ));
            self.updatepage
                .emit(UpdatePageMsg::Update(updateuseritems, updatesystemitems));
        } else {
            error!("Could not connect to pkgdb");
        }
    }
}
