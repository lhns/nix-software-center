use nsc_backend::profile::{installed, parse_list_json, store_version};

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

#[test]
fn stock_store_path_only_element_is_hidden() {
    let els = parse_list_json(&fixture("stock-v3-store-only.json")).unwrap();
    assert_eq!(els.len(), 1);
    assert_eq!(els[0].name, "home-manager-path");
    assert_eq!(els[0].active, Some(true));
    assert!(installed(&els).is_empty());
}

#[test]
fn declared_elements_and_backends() {
    let els = parse_list_json(&fixture("declared-backends.json")).unwrap();
    assert_eq!(els.len(), 4);
    let hello = els.iter().find(|e| e.name == "hello").unwrap();
    assert_eq!(hello.backend.as_deref(), Some("declaration"));
    assert!(hello.store_paths.is_empty());
    assert_eq!(hello.outputs, None);

    let pkgs = installed(&els);
    let keys: Vec<_> = pkgs.iter().map(|p| p.key.as_str()).collect();
    assert_eq!(keys, ["hello", "jq", "github:owner/repo#packages.x86_64-linux.default"]);

    assert_eq!(pkgs[0].element, "hello");
    assert_eq!(pkgs[0].nixpkgs_attr.as_deref(), Some("hello"));
    assert_eq!(pkgs[0].store_version, None);
    assert_eq!(pkgs[1].store_version.as_deref(), Some("1.7.1-bin"));
    assert_eq!(pkgs[2].element, "mytool");
    assert_eq!(pkgs[2].nixpkgs_attr, None);
}

#[test]
fn name_collision_keeps_both_elements() {
    let pkgs = installed(&parse_list_json(&fixture("name-collision.json")).unwrap());
    let elements: Vec<_> = pkgs.iter().map(|p| p.element.as_str()).collect();
    assert_eq!(elements, ["hello", "hello-1"]);
    assert_eq!(pkgs[1].store_version.as_deref(), Some("2.10"));
}

#[test]
fn system_segment_is_dropped() {
    let a = installed(&parse_list_json(&fixture("aarch64.json")).unwrap());
    let x = installed(&parse_list_json(&fixture("name-collision.json")).unwrap());
    assert_eq!(a[0].key, x[0].key);
    assert_eq!(a[0].key, "hello");
}

#[test]
fn old_list_form_uses_index_as_name() {
    let els = parse_list_json(&fixture("old-list.json")).unwrap();
    assert_eq!(els.len(), 2);
    assert_eq!(els[0].name, "0");
    assert_eq!(els[1].name, "1");
    let pkgs = installed(&els);
    assert_eq!(pkgs.len(), 1);
    assert_eq!(pkgs[0].element, "0");
    assert_eq!(pkgs[0].key, "hello");
}

#[test]
fn recorded_nix_2_34_9_output() {
    let els = parse_list_json(&fixture("recorded-2.34.9-profile-list.json")).unwrap();
    let pkgs = installed(&els);
    assert_eq!(pkgs.len(), 1);
    assert_eq!(pkgs[0].element, "flake");
    assert_eq!(pkgs[0].key, "path:/work/flake#packages.x86_64-linux.default");
    assert_eq!(pkgs[0].store_version.as_deref(), Some("1.0"));
    let nix = els.iter().find(|e| e.name == "nix").unwrap();
    assert_eq!(nix.priority, Some(5));
}

#[test]
fn rejects_garbage() {
    assert!(parse_list_json("not json").is_err());
    assert!(parse_list_json(r#"{"version":3}"#).is_err());
}

#[test]
fn store_version_follows_parse_drv_name() {
    let p = |n: &str| store_version(&format!("/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-{n}"));
    assert_eq!(p("hello-2.12.1").as_deref(), Some("2.12.1"));
    assert_eq!(p("bash-interactive-5.3p3").as_deref(), Some("5.3p3"));
    assert_eq!(p("python3.11-foo-1.0").as_deref(), Some("1.0"));
    assert_eq!(p("home-manager-path"), None);
    assert_eq!(p("hello-"), None);
    assert_eq!(store_version("/nix/store/short"), None);
}

#[test]
fn only_stock_nix_runs_one_command_at_a_time() {
    let mut nix = nsc_backend::ops::Nix::new("nix");
    assert_eq!(nix.slots(), 1);
    nix.locking_commits = true;
    assert_eq!(nix.slots(), nsc_backend::queue::MAX_SLOTS);
}

#[test]
fn the_sqlite_busy_line_is_noise() {
    use nsc_backend::ops::is_noise;
    assert!(is_noise("warning: SQLite database '/nix/var/nix/db/db.sqlite' is busy"));
    assert!(!is_noise("error: builder for 'x.drv' failed"));
}
