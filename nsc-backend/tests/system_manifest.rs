use nsc_backend::system_manifest::{is_included, parse, read, resolve};
use sqlx::sqlite::SqlitePoolOptions;
use std::collections::HashSet;

async fn pool() -> sqlx::SqlitePool {
    let pool = SqlitePoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();
    sqlx::query("CREATE TABLE pkgs (attribute TEXT NOT NULL PRIMARY KEY, pname TEXT NOT NULL DEFAULT '', version TEXT NOT NULL DEFAULT '', system TEXT NOT NULL DEFAULT '')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO pkgs (attribute, pname) VALUES ('hello','hello'), ('kdePackages.dolphin','dolphin'), ('git','git'), ('gitMinimal','git'), ('a.x','multi'), ('b.y','multi'), ('src:hello','hello'), ('src:only','onlythird'), ('quoted','x'' OR ''1''=''1')")
        .execute(&pool)
        .await
        .unwrap();
    pool
}

fn set(v: &[&str]) -> HashSet<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn parses_packages_and_skips_bad_entries() {
    let json = r#"{"version":1,"unknown":true,"packages":[
        {"pname":"dolphin","version":"1","path":"/nix/store/aaa-dolphin-1"},
        {"pname":"dolphin"}, {"pname":""}, {"pname":5}, {"version":"1"}, 7, null, "str",
        {"pname":"a\nb"}, {"pname":"<b>x</b> & \"q\""}]}"#;
    assert_eq!(parse(json).unwrap(), ["dolphin", "<b>x</b> & \"q\""]);
}

#[test]
fn malformed_json_is_an_error() {
    for bad in ["", "{", "null", "42", r#"{"packages":"x"}"#, r#"{"packages":{"pname":"a"}}"#] {
        assert!(parse(bad).is_err(), "{bad}");
    }
    assert_eq!(parse("{}").unwrap(), Vec::<String>::new());
}

#[test]
fn oversized_name_is_skipped_and_missing_file_errors() {
    let long = "x".repeat(300);
    assert!(parse(&format!(r#"{{"packages":[{{"pname":"{long}"}}]}}"#)).unwrap().is_empty());
    let d = tempfile::tempdir().unwrap();
    assert!(read(&d.path().join("nope.json")).is_err());
    let f = d.path().join("m.json");
    std::fs::write(&f, r#"{"packages":[{"pname":"hello"}]}"#).unwrap();
    assert_eq!(read(&f).unwrap(), ["hello"]);
    assert!(read(d.path()).is_err());
}

#[tokio::test]
async fn matches_by_name_and_never_third_party() {
    let p = pool().await;
    let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    // Last path component wins over aliases sharing the pname; a third-party `src:hello` never matches.
    assert_eq!(resolve(&p, &names(&["hello", "dolphin", "git"])).await, set(&["hello", "kdePackages.dolphin", "git"]));
    // No exact attribute: every built-in candidate; third-party-only names match nothing.
    assert_eq!(resolve(&p, &names(&["multi", "onlythird", "absent"])).await, set(&["a.x", "b.y"]));
    // Hostile names are only ever bound parameters.
    assert_eq!(resolve(&p, &names(&["x' OR '1'='1", "%", "_"])).await, set(&["quoted"]));
}

#[test]
fn own_install_wins_over_the_image() {
    let inc = set(&["hello"]);
    assert!(is_included(&inc, "hello", false));
    assert!(!is_included(&inc, "hello", true));
    assert!(!is_included(&inc, "src:hello", false));
    assert!(!is_included(&HashSet::new(), "hello", false));
}
