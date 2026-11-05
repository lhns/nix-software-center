use super::*;

impl AppModel {
    pub(super) async fn search(&mut self, sender: &ComponentSender<AppModel>, search: String) {
        info!("AppMsg::Search");
        debug!("Searching for: {}", search);
        self.viewstack.set_visible_child_name("search");
        self.set_searchquery(search.to_string());
        let installeduserpkgs = self.installeduserpkgs.clone();
        let installedsystempkgs = self.installedsystempkgs.clone();
        let userpkgtype = self.userpkgtype.clone();
        let pkgdb = self.pkgdb.clone();
        let appdata = self.appdata.clone();
        sender.command(move |out, shutdown| {
            let search = search.clone();
            let installeduserpkgs = installeduserpkgs.clone();
            let installedsystempkgs = installedsystempkgs;
            let userpkgtype = userpkgtype.clone();
            shutdown.register(async move {
                let searchsplit: Vec<String> = search.split(' ').filter(|x| x.len() > 1).map(|x| x.to_string()).collect();
                warn!("Searchsplit: {:?}", searchsplit);
                if let Ok(pkgpool) = &SqlitePool::connect(&format!("sqlite://{}", pkgdb)).await {
                    let mut queryb: QueryBuilder<Sqlite> = QueryBuilder::new(
                        "SELECT pkgs.attribute, pkgs.pname, description, version FROM listed_pkgs AS pkgs JOIN meta ON (pkgs.attribute = meta.attribute) WHERE (",
                    );
                    for (i, q) in searchsplit.iter().enumerate() {
                        if i == searchsplit.len() - 1 {
                            queryb
                                .push(r#"pkgs.attribute LIKE "#)
                                .push_bind(format!("%{}%", q))
                                .push(r#" OR description LIKE "#)
                                .push_bind(format!("%{}%", q))
                                .push(")");
                        } else {
                            queryb
                                .push(r#"pkgs.attribute LIKE "#)
                                .push_bind(format!("%{}%", q))
                                .push(r#" OR description LIKE "#)
                                .push_bind(format!("%{}%", q))
                                .push(r#") AND ("#);
                        }
                    }
                    queryb.push("ORDER BY LENGTH(pkgs.attribute) ASC");
                    let q: Vec<(String, String, String, String)> =
                        queryb.build_query_as().fetch_all(pkgpool).await.unwrap();
                    let mut outpkgs = Vec::new();
                    for (i, (attr, pname, desc, _version)) in q.into_iter().enumerate() {
                        if let Some(data) = appdata.get(&attr) {
                            outpkgs.push(SearchItem {
                                pkg: attr.to_string(),
                                pname: pname.to_string(),
                                name: if let Some(name) = &data.name { name.get("C").unwrap_or(&attr).to_string() } else { attr.to_string() },
                                summary: if desc.is_empty() { None } else { Some(desc) },
                                icon: data
                                    .icon
                                    .as_ref()
                                    .and_then(|x| x.cached.as_ref())
                                    .map(|x| x[0].name.clone()),
                                installeduser: match userpkgtype {
                                  UserPkgs::Env => installeduserpkgs.contains_key(&pname),
                                  UserPkgs::Profile => installeduserpkgs.contains_key(&attr)
                                },
                                installedsystem: installedsystempkgs.contains(&attr),
                            })
                        } else {
                            outpkgs.push(SearchItem {
                                pkg: attr.to_string(),
                                pname: pname.to_string(),
                                name: pname.to_string(),
                                summary: if desc.is_empty() { None } else { Some(desc) },
                                icon: None,
                                installeduser: match userpkgtype {
                                  UserPkgs::Env => installeduserpkgs.contains_key(&pname),
                                  UserPkgs::Profile => installeduserpkgs.contains_key(&attr)
                                },
                                installedsystem: installedsystempkgs.contains(&attr),
                            });
                        }
                        if i >= 200 {
                            break;
                        }
                    }
                    outpkgs.sort_by(|a, b| {
                        let mut aleft = a.name.to_lowercase() + &a.pkg.to_lowercase();
                        let mut bleft = b.name.to_lowercase() + &b.pkg.to_lowercase();
                        for q in searchsplit.iter() {
                            let q = &q.to_lowercase();
                            if aleft.contains(q) {
                                aleft = aleft.replace(q, "");
                            } else {
                                aleft.push_str(q);
                            }
                            if bleft.contains(q) {
                                bleft = bleft.replace(q, "");
                            } else {
                                bleft.push_str(q);
                            }
                        }
                        let mut apoints = aleft.len() + 5;
                        let mut bpoints = bleft.len() + 5;
                        // for q in searchsplit.iter() {
                        //     if a.name.contains(q) {
                        //         apoints -= 1;
                        //     }
                        //     if b.name.contains(q) {
                        //         bpoints -= 1;
                        //     }
                        // }
                        if appdata.get(&a.pkg).is_some() {
                            apoints -= 5;
                        }
                        if appdata.get(&b.pkg).is_some() {
                            bpoints -= 5;
                        }
                        apoints.cmp(&bpoints)
                    });
                    out.send(AppAsyncMsg::Search(search.to_string(), outpkgs));
                }
            }).drop_on_shutdown()
        })
    }
}
