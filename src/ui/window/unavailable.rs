use super::*;

impl AppModel {
    pub(super) async fn get_unavailable_items(&mut self, sender: &ComponentSender<AppModel>, userpkgs: HashMap<String, String>, syspkgs: HashMap<String, String>, updatetype: UpdateType) {
        info!("AppMsg::GetUnavailableItems");
        let appdata: HashMap<String, AppData> = self
            .appdata
            .iter()
            .filter_map(|(k, v)| {
                if syspkgs.contains_key(k) || userpkgs.contains_key(k) {
                    Some((k.to_string(), v.clone()))
                } else {
                    None
                }
            })
            .collect();
        let poolref = self.pkgdb.clone();
        relm4::spawn(async move {
            let mut unavailableuser = vec![];
            let mut unavailablesys = vec![];
            if let Ok(pool) = &SqlitePool::connect(&format!("sqlite://{}", poolref)).await {
                let mut sortuserpkgs = userpkgs.into_iter().collect::<Vec<_>>();
                sortuserpkgs.sort();
                for (pkg, msg) in sortuserpkgs {
                    if let Some(data) = appdata.get(&pkg) {
                        let pname: Result<(String,), sqlx::Error> =
                            sqlx::query_as("SELECT pname FROM pkgs WHERE attribute = $1")
                                .bind(&pkg)
                                .fetch_one(pool)
                                .await;
                        if let Ok(pname) = pname {
                            unavailableuser.push(UnavailableItemModel {
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
                                message: msg,
                            })
                        } else {
                            unavailableuser.push(UnavailableItemModel {
                                pkg: pkg.to_string(),
                                name: if let Some(name) = &data.name {
                                    name.get("C").unwrap_or(&pkg).to_string()
                                } else {
                                    pkg.to_string()
                                },
                                pname: String::new(),
                                icon: data
                                    .icon
                                    .as_ref()
                                    .and_then(|x| x.cached.as_ref())
                                    .map(|x| x[0].name.clone()),
                                message: msg,
                            })
                        }
                    } else {
                        unavailableuser.push(UnavailableItemModel {
                            pkg: pkg.to_string(),
                            name: pkg.to_string(),
                            pname: String::new(),
                            icon: None,
                            message: msg,
                        })
                    }
                }
                let mut sortsyspkgs = syspkgs.into_iter().collect::<Vec<_>>();
                sortsyspkgs.sort();
                for (pkg, msg) in sortsyspkgs {
                    if let Some(data) = appdata.get(&pkg) {
                        let pname: Result<(String,), sqlx::Error> =
                            sqlx::query_as("SELECT pname FROM pkgs WHERE attribute = $1")
                                .bind(&pkg)
                                .fetch_one(pool)
                                .await;
                        if let Ok(pname) = pname {
                            unavailablesys.push(UnavailableItemModel {
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
                                message: msg,
                            })
                        } else {
                            unavailablesys.push(UnavailableItemModel {
                                pkg: pkg.to_string(),
                                name: if let Some(name) = &data.name {
                                    name.get("C").unwrap_or(&pkg).to_string()
                                } else {
                                    pkg.to_string()
                                },
                                pname: String::new(),
                                icon: data
                                    .icon
                                    .as_ref()
                                    .and_then(|x| x.cached.as_ref())
                                    .map(|x| x[0].name.clone()),
                                message: msg,
                            })
                        }
                    } else {
                        unavailablesys.push(UnavailableItemModel {
                            pkg: pkg.to_string(),
                            name: pkg.to_string(),
                            pname: String::new(),
                            icon: None,
                            message: msg,
                        })
                    }
                }
            }
            UNAVAILABLE_BROKER.send(UnavailableDialogMsg::Show(
                unavailableuser,
                unavailablesys,
                updatetype,
            ));
        });
    }
}
