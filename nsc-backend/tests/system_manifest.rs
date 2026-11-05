use nsc_backend::system_manifest::{is_included, parse, read, resolve, Entry};
use sqlx::sqlite::SqlitePoolOptions;
use std::collections::HashSet;

/// (attribute, pname, version); the shapes come from a real image's catalogue.
const ROWS: &[(&str, &str, &str)] = &[
    ("hello", "hello", "2.12"),
    ("kdePackages.dolphin", "dolphin", "25.08"),
    ("kdePackages.kwin", "kwin", "6.4"),
    ("git", "git", "2.50"),
    ("gitMinimal", "git", "2.50"),
    ("gnupatch", "patch", "2.8"),
    ("haskellPackages.patch", "patch", "0.0.8"),
    ("texlivePackages.patch", "patch", "svn1"),
    ("kdePackages.spectacle", "spectacle", "25.08"),
    ("haskellPackages.spectacle", "spectacle", "0.1"),
    ("dbus", "dbus", "1.14"),
    ("haskellPackages.dbus", "dbus", "1.3"),
    ("tcl8Packages.dbus", "dbus", "1.14"),
    ("curlFull", "curl", "8.14"),
    ("curlMinimal", "curl", "8.14"),
    ("azure-sdk-for-cpp.curl", "curl", "1.0"),
    ("haskellPackages.curl", "curl", "1.3"),
    ("systemd", "systemd", "257"),
    ("systemdUkify", "systemd", "257"),
    ("linuxKernel.packages.linux_6_6.systemd", "systemd", "257"),
    ("a.x", "multi", "1"),
    ("b.y", "multi", "2"),
    ("src:hello", "hello", "2.12"),
    ("src:only", "onlythird", "1"),
    ("quoted", "x' OR '1'='1", "1"),
];

async fn pool() -> sqlx::SqlitePool {
    let pool = SqlitePoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();
    sqlx::query("CREATE TABLE pkgs (attribute TEXT NOT NULL PRIMARY KEY, pname TEXT NOT NULL DEFAULT '', version TEXT NOT NULL DEFAULT '', system TEXT NOT NULL DEFAULT '')")
        .execute(&pool)
        .await
        .unwrap();
    for (a, p, v) in ROWS {
        sqlx::query("INSERT INTO pkgs (attribute, pname, version) VALUES ($1, $2, $3)")
            .bind(a)
            .bind(p)
            .bind(v)
            .execute(&pool)
            .await
            .unwrap();
    }
    pool
}

fn set(v: &[&str]) -> HashSet<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn e(pname: &str, version: &str) -> Entry {
    Entry { pname: pname.into(), version: version.into() }
}

async fn one(pname: &str, version: &str) -> HashSet<String> {
    resolve(&pool().await, &[e(pname, version)]).await
}

#[test]
fn parses_packages_and_skips_bad_entries() {
    let json = r#"{"version":1,"unknown":true,"packages":[
        {"pname":"dolphin","version":"1","path":"/nix/store/aaa-dolphin-1"},
        {"pname":"dolphin","version":"1"}, {"pname":"dolphin","version":2}, {"pname":"dolphin"},
        {"pname":""}, {"pname":5}, {"version":"1"}, 7, null, "str",
        {"pname":"a\nb"}, {"pname":"<b>x</b> & \"q\"","version":"v\u0000"}]}"#;
    assert_eq!(
        parse(json).unwrap(),
        [e("dolphin", "1"), e("dolphin", ""), e("<b>x</b> & \"q\"", "")]
    );
}

#[test]
fn malformed_json_is_an_error() {
    for bad in ["", "{", "null", "42", r#"{"packages":"x"}"#, r#"{"packages":{"pname":"a"}}"#] {
        assert!(parse(bad).is_err(), "{bad}");
    }
    assert_eq!(parse("{}").unwrap(), Vec::<Entry>::new());
}

#[test]
fn oversized_name_is_skipped_and_missing_file_errors() {
    let long = "x".repeat(300);
    assert!(parse(&format!(r#"{{"packages":[{{"pname":"{long}"}}]}}"#)).unwrap().is_empty());
    let d = tempfile::tempdir().unwrap();
    assert!(read(&d.path().join("nope.json")).is_err());
    let f = d.path().join("m.json");
    std::fs::write(&f, r#"{"packages":[{"pname":"hello"}]}"#).unwrap();
    assert_eq!(read(&f).unwrap(), [e("hello", "")]);
    assert!(read(d.path()).is_err());
}

#[tokio::test]
async fn version_picks_among_attributes_sharing_a_pname() {
    assert_eq!(one("dolphin", "25.08").await, set(&["kdePackages.dolphin"]));
    // The package sets' lookalikes have other versions.
    assert_eq!(one("spectacle", "25.08").await, set(&["kdePackages.spectacle"]));
    assert_eq!(one("patch", "2.8").await, set(&["gnupatch"]));
    // dbus: the tcl one has the same version but is a set member; top-level wins.
    assert_eq!(one("dbus", "1.14").await, set(&["dbus"]));
    // The variants stay, the language bindings go.
    assert_eq!(one("curl", "8.14").await, set(&["curlFull", "curlMinimal"]));
    // Top-level first, then the one named like the pname.
    assert_eq!(one("systemd", "257").await, set(&["systemd"]));
    assert_eq!(one("git", "2.50").await, set(&["git"]));
}

#[tokio::test]
async fn no_version_match_falls_back_only_for_a_single_candidate() {
    // Patched package: one candidate, other version.
    assert_eq!(one("kwin", "6.4-patched").await, set(&["kdePackages.kwin"]));
    assert_eq!(one("kwin", "").await, set(&["kdePackages.kwin"]));
    // Several candidates, none with that version: nothing rather than a guess.
    assert!(one("patch", "2.7").await.is_empty());
    assert!(one("patch", "").await.is_empty());
    assert!(one("spectacle", "9.9").await.is_empty());
    assert!(one("multi", "3").await.is_empty());
    assert_eq!(one("multi", "2").await, set(&["b.y"]));
}

#[tokio::test]
async fn never_third_party_and_hostile_names_are_data() {
    assert_eq!(one("hello", "2.12").await, set(&["hello"]));
    assert!(one("onlythird", "1").await.is_empty());
    assert!(one("absent", "1").await.is_empty());
    assert_eq!(one("x' OR '1'='1", "1").await, set(&["quoted"]));
    assert!(resolve(&pool().await, &[e("%", ""), e("_", "")]).await.is_empty());
}

#[test]
fn own_install_wins_over_the_image() {
    let inc = set(&["hello"]);
    assert!(is_included(&inc, "hello", false));
    assert!(!is_included(&inc, "hello", true));
    assert!(!is_included(&inc, "src:hello", false));
    assert!(!is_included(&HashSet::new(), "hello", false));
}
