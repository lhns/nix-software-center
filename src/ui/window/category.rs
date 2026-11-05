use super::*;

impl AppModel {
    pub(super) async fn update_rec_pkgs(&mut self, sender: &ComponentSender<AppModel>, pkgs: Vec<String>) {
        info!("AppMsg::UpdateRecPkgs");
        let appdata: HashMap<String, AppData> = self
            .appdata
            .iter()
            .filter_map(|(k, v)| {
                if pkgs.contains(k) {
                    Some((k.to_string(), v.clone()))
                } else {
                    None
                }
            })
            .collect();
        let installeduser = self.installeduserpkgs.clone();
        let installedsystem = self.installedsystempkgs.clone();
        let poolref = self.pkgdb.clone();
        let userpkgtype = self.userpkgtype.clone();
        sender.oneshot_command(async move {
            let mut pkgtiles = vec![];
            if let Ok(pool) = &SqlitePool::connect(&format!("sqlite://{}", poolref)).await {
                for pkg in pkgs {
                    if let Some(data) = appdata.get(&pkg) {
                        let pname: (String,) =
                            sqlx::query_as("SELECT pname FROM pkgs WHERE attribute = $1")
                                .bind(&pkg)
                                .fetch_one(pool)
                                .await
                                .unwrap();
                        pkgtiles.push(PkgTile {
                            pkg: pkg.to_string(),
                            name: if let Some(name) = &data.name {
                                name.get("C").unwrap_or(&pname.0).to_string()
                            } else {
                                pname.0.to_string()
                            },
                            pname: pname.0.to_string(),
                            icon: data
                                .icon
                                .as_ref()
                                .and_then(|x| x.cached.as_ref())
                                .map(|x| x[0].name.clone()),
                            summary: data
                                .summary
                                .as_ref()
                                .and_then(|x| x.get("C"))
                                .map(|x| x.to_string())
                                .unwrap_or_default(),
                            installeduser: installeduser.contains_key(&match userpkgtype {
                                UserPkgs::Env => pname.0,
                                UserPkgs::Profile => pkg.to_string(),
                            }),
                            installedsystem: installedsystem.contains(&pkg),
                        })
                    } else if let Ok((pname, description)) = sqlx::query_as::<_, (String, String)>(
                        "SELECT pname, description FROM pkgs JOIN meta ON (pkgs.attribute = meta.attribute) WHERE pkgs.attribute = $1",
                    )
                    .bind(&pkg)
                    .fetch_one(pool)
                    .await
                    {
                        // Not in AppStream (third-party apps): the catalogue is all there is.
                        pkgtiles.push(PkgTile {
                            pkg: pkg.to_string(),
                            name: pname.clone(),
                            pname,
                            icon: None,
                            summary: description,
                            installeduser: installeduser.contains_key(&pkg),
                            installedsystem: installedsystem.contains(&pkg),
                        })
                    }
                }
            }
            AppAsyncMsg::UpdateRecPkgs(pkgtiles)
        });
    }

    pub(super) async fn load_category(&mut self, sender: &ComponentSender<AppModel>, category: PkgCategory) {
        info!("AppMsg::LoadCategory({:?})", category);
        let pkgdb = self.pkgdb.clone();
        let categoryrec = self.categoryrec.get(&category).unwrap_or(&vec![]).to_vec();
        let categoryall = self.categoryall.get(&category).unwrap_or(&vec![]).to_vec();
        let appdata = self.appdata.clone();
        let installeduser = self.installeduserpkgs.clone();
        let installedsystem = self.installedsystempkgs.clone();
        let category = category;
        sender.oneshot_command(async move {
            let mut catrec = vec![];
            let mut catall = vec![];
            if let Ok(pool) = &SqlitePool::connect(&format!("sqlite://{}", pkgdb)).await {
                for pkg in categoryrec {
                    if let Some(data) = appdata.get(&pkg) {
                        let pname: (String,) =
                        sqlx::query_as("SELECT pname FROM pkgs WHERE attribute = $1")
                            .bind(&pkg)
                            .fetch_one(pool)
                            .await
                            .unwrap();
                        catrec.push(CategoryTile {
                            pkg: pkg.to_string(),
                            name: if let Some(name) = &data.name {
                                name.get("C").unwrap_or(&pname.0).to_string()
                            } else {
                                pname.0.to_string()
                            },
                            pname: pname.0,
                            icon: data
                                .icon
                                .as_ref()
                                .and_then(|x| x.cached.as_ref())
                                .map(|x| x[0].name.clone()),
                            summary: data
                                .summary
                                .as_ref()
                                .and_then(|x| x.get("C"))
                                .map(|x| x.to_string()),
                            installeduser: installeduser.contains_key(&pkg),
                            installedsystem: installedsystem.contains(&pkg),
                        })
                    } else {
                        let (pname, description): (String, String) =
                        sqlx::query_as("SELECT pname, description FROM pkgs JOIN meta ON (pkgs.attribute = meta.attribute) WHERE pkgs.attribute = $1")
                            .bind(&pkg)
                            .fetch_one(pool)
                            .await
                            .unwrap();
                        catrec.push(CategoryTile {
                            pkg: pkg.to_string(),
                            name: pname.to_string(),
                            pname: pname.to_string(),
                            icon: None,
                            summary: if description.is_empty() { None } else { Some(description) },
                            installeduser: installeduser.contains_key(&pkg),
                            installedsystem: installedsystem.contains(&pkg),
                        })
                    }
                }
                for pkg in categoryall {
                    if let Some(data) = appdata.get(&pkg) {
                        let pname: (String,) =
                        sqlx::query_as("SELECT pname FROM pkgs WHERE attribute = $1")
                            .bind(&pkg)
                            .fetch_one(pool)
                            .await
                            .unwrap();
                        catall.push(CategoryTile {
                            pkg: pkg.to_string(),
                            name: if let Some(name) = &data.name {
                                name.get("C").unwrap_or(&pname.0).to_string()
                            } else {
                                pname.0.to_string()
                            },
                            pname: pname.0,
                            icon: data
                                .icon
                                .as_ref()
                                .and_then(|x| x.cached.as_ref())
                                .map(|x| x[0].name.clone()),
                            summary: data
                                .summary
                                .as_ref()
                                .and_then(|x| x.get("C"))
                                .map(|x| x.to_string()),
                            installeduser: installeduser.contains_key(&pkg),
                            installedsystem: installedsystem.contains(&pkg),
                        })
                    } else {
                        let (pname, description): (String, String) =
                        sqlx::query_as("SELECT pname, description FROM pkgs JOIN meta ON (pkgs.attribute = meta.attribute) WHERE pkgs.attribute = $1")
                            .bind(&pkg)
                            .fetch_one(pool)
                            .await
                            .unwrap();
                        catall.push(CategoryTile {
                            pkg: pkg.to_string(),
                            name: pname.to_string(),
                            pname: pname.to_string(),
                            icon: None,
                            summary: if description.is_empty() { None } else { Some(description) },
                            installeduser: installeduser.contains_key(&pkg),
                            installedsystem: installedsystem.contains(&pkg),
                        })
                    }
                }
            } else {
                error!("Failed to connect to pkgdb")
            }
            AppAsyncMsg::LoadCategory(category, catrec, catall)
        });
    }
}
