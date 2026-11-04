use nsc_backend::profile::{installed, installed_in, parse_list_json};
use nsc_backend::sources::*;

#[test]
fn flake_ref_validation() {
    let ok = [
        "nixpkgs",
        "flake:nixpkgs-unfree",
        "github:owner/repo",
        "github:owner/repo/branch?dir=nix/x",
        "gitlab:owner/repo",
        "sourcehut:~user/repo",
        "git+https://example.org/r.git?ref=main",
        "https://example.org/a.tar.gz",
        "github:lhns/nix-software-center?dir=nix/unfree",
    ];
    for r in ok {
        assert_eq!(validate_flake_ref(r), Ok(()), "{r}");
    }
    let long = format!("github:o/{}", "a".repeat(600));
    let bad = [
        "", "-github:o/r", "--flag", "github:o/r x", "github:o/r\n", "github:o/r;ls", "github:o/r`id`",
        "github:o/$(id)", "github:o/r'", "github:o/r\"", "github:o/r|x", "github:o/r&x", "github:o/r#attr",
        "github:o/r\\", "path:/etc", "git+ssh://h/r", "file:///x", "http://x/y", "ftp://x", "a b", "github:",
        "has/slash", ".", "..", ".hidden", &long,
    ];
    for r in bad {
        assert!(validate_flake_ref(r).is_err(), "{r:?} should be refused");
    }
}

#[test]
fn names_and_installables() {
    assert!(valid_name("python3Packages.foo-bar_1+x"));
    for n in ["", "a b", "a;b", "a$b", "a'b", "a/b", "a#b"] {
        assert!(!valid_name(n), "{n:?}");
    }
    assert_eq!(installable("nixpkgs", "hello").unwrap(), "nixpkgs#hello");
    assert!(installable("nixpkgs", "x;y").is_err());
    assert!(installable("-x", "y").is_err());
}

#[test]
fn ids_are_generated_and_unique() {
    assert_eq!(new_id("My Source!", &[]), "my-source");
    assert_eq!(new_id("<b>&", &[]), "b");
    assert_eq!(new_id("???", &[]), "source");
    let taken = vec!["nixpkgs".to_string(), "nixpkgs-2".to_string()];
    assert_eq!(new_id("nixpkgs", &taken), "nixpkgs-3");
    assert!(is_id("a-b-1") && !is_id("A") && !is_id("a:b") && !is_id(""));
}

fn settings(default_unfree: bool) -> Settings {
    Settings {
        system_packages: true,
        unfree_flake: "nixpkgs-unfree".into(),
        unfree_listed_default: default_unfree,
    }
}

fn store(dir: &std::path::Path, default_unfree: bool) -> Store {
    Store::load(dir.join("sources.json"), settings(default_unfree))
}

fn on(s: &Store, id: &str) -> bool {
    s.sources().iter().find(|x| x.id == id).unwrap().enabled
}

#[test]
fn absent_toggle_follows_the_default_and_explicit_survives() {
    let d = tempfile::tempdir().unwrap();
    let mut s = store(d.path(), false);
    assert!(on(&s, "nixpkgs") && !on(&s, "nixpkgs-unfree"));
    let id = s.add("Mine", "github:o/r").unwrap();
    assert!(on(&s, &id));
    s.save().unwrap();
    // The deployer flips the default: the untouched built-in follows.
    assert!(on(&store(d.path(), true), "nixpkgs-unfree"));
    // An explicit choice survives either default.
    let mut s3 = store(d.path(), true);
    s3.set_enabled("nixpkgs-unfree", false);
    s3.save().unwrap();
    assert!(!on(&store(d.path(), true), "nixpkgs-unfree"));
    assert!(!on(&store(d.path(), false), "nixpkgs-unfree"));
    assert_eq!(store(d.path(), true).sources().len(), 3);
    let text = std::fs::read_to_string(d.path().join("sources.json")).unwrap();
    assert!(!text.contains("\"nixpkgs\":"), "untouched built-in must not be written: {text}");
}

#[test]
fn bad_file_is_kept_until_the_next_change() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("sources.json");
    std::fs::write(&p, "{ nope").unwrap();
    let mut s = store(d.path(), true);
    assert!(s.parse_error.is_some());
    assert_eq!(s.sources().len(), 2);
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "{ nope");
    s.add("x", "github:o/r").unwrap();
    s.save().unwrap();
    assert_eq!(std::fs::read_to_string(d.path().join("sources.json.bad")).unwrap(), "{ nope");
    assert_eq!(store(d.path(), true).sources().len(), 3);
}

#[test]
fn hand_edited_entries_are_sanitised() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("sources.json"),
        r#"{"sources":[{"id":"ok","name":"n","flake":"github:o/r"},{"id":"Bad Id","name":"n","flake":"github:o/r"},
        {"id":"x","name":"n","flake":"--evil"},{"id":"nixpkgs","name":"n","flake":"github:o/r"}]}"#,
    )
    .unwrap();
    let ids: Vec<_> = store(d.path(), true).sources().into_iter().map(|s| s.id).collect();
    assert_eq!(ids, ["nixpkgs", "nixpkgs-unfree", "ok"]);
}

#[test]
fn installed_elements_map_to_sources() {
    let els = parse_list_json(
        r#"{"version":3,"elements":{
        "hello":{"attrPath":"legacyPackages.x86_64-linux.hello","originalUrl":"flake:nixpkgs","storePaths":[]},
        "slack":{"attrPath":"legacyPackages.x86_64-linux.slack","originalUrl":"flake:nixpkgs-unfree","storePaths":[]},
        "tool":{"attrPath":"packages.x86_64-linux.tool","originalUrl":"github:o/r","storePaths":[]},
        "tool2":{"attrPath":"legacyPackages.x86_64-linux.tool2","originalUrl":"github:o/r","storePaths":[]},
        "gone":{"attrPath":"packages.x86_64-linux.gone","originalUrl":"github:removed/src","storePaths":[]},
        "gone2":{"attrPath":"legacyPackages.x86_64-linux.gone2","originalUrl":"github:removed/src","storePaths":[]},
        "manual":{"attrPath":"legacyPackages.x86_64-linux.manual","originalUrl":"github:NixOS/nixpkgs/nixos-unstable","storePaths":[]}}}"#,
    )
    .unwrap();
    let mut s = Store { path: "never-written.json".into(), file: Default::default(), settings: settings(true), parse_error: None };
    assert_eq!(s.add("Mine", "github:o/r").unwrap(), "mine");
    let srcs = s.sources();
    let got = installed_in(&els, Some((&srcs, "nixpkgs-unfree")));
    let view: Vec<_> = got.iter().map(|p| (p.key.as_str(), p.nixpkgs_attr.is_some(), p.source_removed)).collect();
    assert_eq!(
        view,
        [
            ("github:removed/src#packages.x86_64-linux.gone", false, true),
            ("github:removed/src#legacyPackages.x86_64-linux.gone2", false, true),
            ("hello", true, false),
            ("manual", true, false),
            ("slack", true, false),
            ("mine:tool", true, false),
            ("mine:tool2", true, false),
        ]
    );
    // Without sources the old rule holds: every legacyPackages element is a nixpkgs attribute.
    let old = installed(&els);
    assert!(old.iter().any(|p| p.key == "gone2" && p.nixpkgs_attr.is_some() && !p.source_removed));
}
