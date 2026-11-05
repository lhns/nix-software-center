use super::*;

impl AppModel {
    pub(super) async fn open_pkg(&mut self, sender: &ComponentSender<AppModel>, pkg: String) {
        info!("AppMsg::OpenPkg {}", pkg);
        sender.input(AppMsg::CheckNetwork);
        if let Ok(pool) = &SqlitePool::connect(&format!("sqlite://{}", self.pkgdb)).await {
            let pkgdata: Result<
                (
                    String,
                    String,
                    String,
                    String,
                    String,
                    String,
                    String,
                    String,
                    String,
                ),
                _,
            > = sqlx::query_as(
                r#"
SELECT pname, version, system, description, longdescription, homepage, license, platforms, maintainers
FROM pkgs JOIN meta ON (pkgs.attribute = meta.attribute) WHERE pkgs.attribute = $1
            "#,
            )
            .bind(&pkg)
            .fetch_one(pool)
            .await;

            if let Ok((
                pname,
                version,
                system,
                description,
                longdescription,
                homepage,
                licensejson,
                platformsjson,
                maintainersjson,
            )) = pkgdata
            {
                let mut name = pname.to_string();
                let mut summary = if description.is_empty() {
                    None
                } else {
                    Some(description)
                };
                let mut description = if longdescription.is_empty() {
                    None
                } else {
                    Some(longdescription)
                };
                let mut icon = None;
                let mut screenshots = vec![];
                let mut licenses = vec![];
                let mut platforms = vec![];
                let mut maintainers = vec![];
                let mut launchable = None;

                if let Some(data) = self.appdata.get(&pkg) {
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
                    if let Some(d) = &data.description {
                        if let Some(d) = d.get("C") {
                            description = Some(d.to_string());
                        }
                    }
                    if let Some(i) = &data.icon {
                        if let Some(mut i) = i.cached.clone() {
                            i.sort_by(|x, y| x.height.cmp(&y.height));
                            if let Some(i) = i.last() {
                                icon = Some(format!(
                                    "{}/icons/nixos/{}x{}/{}",
                                    APPINFO, i.width, i.height, i.name
                                ));
                            }
                        }
                    }
                    if let Some(s) = &data.screenshots {
                        for s in s {
                            if let Some(u) = &s.sourceimage {
                                if !screenshots.contains(&u.url) {
                                    if s.default == Some(true) {
                                        screenshots.insert(0, u.url.clone());
                                    } else {
                                        screenshots.push(u.url.clone());
                                    }
                                } else if s.default == Some(true) {
                                    if let Some(index) =
                                        screenshots.iter().position(|x| *x == u.url)
                                    {
                                        screenshots.remove(index);
                                        screenshots.insert(0, u.url.clone());
                                    }
                                }
                            }
                        }
                    }
                    if let Some(l) = &data.launchable {
                        if let Some(d) = l.desktopid.get(0) {
                            launchable = Some(d.to_string());
                        }
                    }
                }

                fn addlicense(
                    pkglicense: &LicenseEnum,
                    licenses: &mut Vec<pkgpage::License>,
                ) {
                    match pkglicense {
                        LicenseEnum::Single(l) => {
                            if let Some(n) = &l.fullname {
                                let parsed = if let Some(id) = &l.spdxid {
                                    if let Ok(Some(license)) =
                                        Expression::parse(id).map(|p| {
                                            p.requirements()
                                                .map(|er| er.req.license.id())
                                                .collect::<Vec<_>>()[0]
                                        })
                                    {
                                        Some(license)
                                    } else {
                                        None
                                    }
                                } else if let Ok(Some(license)) =
                                    Expression::parse(n).map(|p| {
                                        p.requirements()
                                            .map(|er| er.req.license.id())
                                            .collect::<Vec<_>>()[0]
                                    })
                                {
                                    Some(license)
                                } else {
                                    None
                                };
                                licenses.push(pkgpage::License {
                                    free: if let Some(f) = l.free {
                                        Some(f)
                                    } else {
                                        parsed.map(|p| {
                                            p.is_osi_approved() || p.is_fsf_free_libre()
                                        })
                                    },
                                    fullname: n.to_string(),
                                    spdxid: l.spdxid.clone(),
                                    url: if let Some(u) = &l.url {
                                        Some(u.to_string())
                                    } else {
                                        parsed.map(|p| {
                                            format!(
                                                "https://spdx.org/licenses/{}.html",
                                                p.name
                                            )
                                        })
                                    },
                                })
                            } else if let Some(s) = &l.spdxid {
                                if let Ok(Some(license)) = Expression::parse(s).map(|p| {
                                    p.requirements()
                                        .map(|er| er.req.license.id())
                                        .collect::<Vec<_>>()[0]
                                }) {
                                    licenses.push(pkgpage::License {
                                        free: Some(
                                            license.is_osi_approved()
                                                || license.is_fsf_free_libre()
                                                || l.free.unwrap_or(false),
                                        ),
                                        fullname: license.full_name.to_string(),
                                        spdxid: Some(license.name.to_string()),
                                        url: if l.url.is_some() {
                                            l.url.clone()
                                        } else {
                                            Some(format!(
                                                "https://spdx.org/licenses/{}.html",
                                                license.name
                                            ))
                                        },
                                    })
                                }
                            }
                        }
                        LicenseEnum::List(lst) => {
                            for l in lst {
                                addlicense(&LicenseEnum::Single(l.clone()), licenses);
                            }
                        }
                        LicenseEnum::SingleStr(s) => {
                            if let Ok(Some(license)) = Expression::parse(s).map(|p| {
                                p.requirements()
                                    .map(|er| er.req.license.id())
                                    .collect::<Vec<_>>()[0]
                            }) {
                                licenses.push(pkgpage::License {
                                    free: Some(
                                        license.is_osi_approved()
                                            || license.is_fsf_free_libre(),
                                    ),
                                    fullname: license.full_name.to_string(),
                                    spdxid: Some(license.name.to_string()),
                                    url: Some(format!(
                                        "https://spdx.org/licenses/{}.html",
                                        license.name
                                    )),
                                })
                            }
                        }
                        LicenseEnum::VecStr(lst) => {
                            for s in lst {
                                addlicense(&LicenseEnum::SingleStr(s.clone()), licenses);
                            }
                        }
                        LicenseEnum::Mixed(v) => {
                            for l in v {
                                addlicense(l, licenses);
                            }
                        }
                    }
                }

                if let Ok(pkglicense) = serde_json::from_str::<LicenseEnum>(&licensejson) {
                    addlicense(&pkglicense, &mut licenses);
                }

                let platformslst = serde_json::from_str::<Platform>(&platformsjson);
                if let Ok(p) = platformslst {
                    match p {
                        Platform::Single(p) => {
                            if !platforms.contains(&p) && p != system {
                                platforms.push(p);
                            }
                        }
                        Platform::List(v) => {
                            for p in v {
                                if !platforms.contains(&p.to_string()) && p != system {
                                    platforms.push(p.to_string());
                                }
                            }
                        }
                        Platform::ListList(vv) => {
                            for v in vv {
                                for p in v {
                                    if !platforms.contains(&p.to_string()) && p != system {
                                        platforms.push(p.to_string());
                                    }
                                }
                            }
                        }
                    }
                }
                platforms.sort();
                platforms.insert(0, system);

                if let Ok(m) = serde_json::from_str::<Vec<PkgMaintainer>>(&maintainersjson)
                {
                    for m in m {
                        maintainers.push(m);
                    }
                }

                let out = PkgInitModel {
                    name,
                    version: if version.is_empty() {
                        None
                    } else {
                        Some(version.to_string())
                    },
                    pname,
                    summary,
                    description,
                    icon,
                    pkg,
                    screenshots,
                    homepage: if homepage.is_empty() {
                        None
                    } else {
                        Some(homepage)
                    },
                    platforms,
                    licenses,
                    maintainers,
                    installeduserpkgs: self.installeduserpkgs.keys().cloned().collect(),
                    installedsystempkgs: self.installedsystempkgs.clone(),
                    launchable,
                };
                self.page = Page::PkgPage;
                if self.viewstack.visible_child_name()
                    != Some(gtk::glib::GString::from("search"))
                {
                    self.searching = false;
                }
                self.busy = false;
                self.pkgpage.emit(PkgMsg::Open(Box::new(out)));
            }
        } else {
            error!("No pkgdb!");
        }
    }
}
