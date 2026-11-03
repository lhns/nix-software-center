# Runs pkgdb.nix over this fixture and asserts the schema the app queries rely on.
{
  callPackage,
  runCommand,
  sqlite,
  system,
}:
let
  pkgdb = callPackage ../../pkgdb.nix { path = ./.; inherit system; };
in
runCommand "pkgdb-small-check" { nativeBuildInputs = [ sqlite ]; } ''
  db=${pkgdb}/nixpkgs.db
  q() { sqlite3 -batch "$db" "$1"; }
  eq() { # description, query, expected
    got=$(q "$2")
    if [ "$got" != "$3" ]; then
      echo "FAIL: $1: expected [$3], got [$got]" >&2
      exit 1
    fi
  }

  eq "attributes" "SELECT group_concat(attribute, ' ') FROM (SELECT attribute FROM pkgs ORDER BY 1)" \
    "audioPkgs.synth awkward broken-thing hello list-homepage minimal vulnerable"
  eq "meta rows match pkgs" "SELECT count(*) FROM meta JOIN pkgs USING (attribute)" 7
  eq "pkgs columns" "SELECT group_concat(name) FROM pragma_table_info('pkgs')" "attribute,pname,version,system"
  eq "meta columns" "SELECT group_concat(name) FROM pragma_table_info('meta')" \
    "attribute,description,longdescription,homepage,license,platforms,maintainers,position,broken,insecure"
  eq "pname index" "SELECT count(*) FROM pragma_index_list('pkgs') WHERE name = 'pkgs_pname'" 1
  eq "no NULL or non-text" "SELECT count(*) FROM pkgs JOIN meta USING (attribute)
    WHERE typeof(pname) != 'text' OR typeof(version) != 'text' OR typeof(system) != 'text'
    OR typeof(description) != 'text' OR typeof(longdescription) != 'text' OR typeof(homepage) != 'text'
    OR typeof(license) != 'text' OR typeof(platforms) != 'text' OR typeof(maintainers) != 'text'
    OR typeof(position) != 'text' OR typeof(broken) != 'integer' OR typeof(insecure) != 'integer'" 0
  eq "pname/version" "SELECT pname || ' ' || version FROM pkgs WHERE attribute = 'hello'" "hello 1.0"
  eq "system" "SELECT system FROM pkgs WHERE attribute = 'hello'" "${pkgdb.system}"
  eq "relative position" "SELECT position FROM meta WHERE attribute = 'audioPkgs.synth'" \
    "pkgs/applications/audio/synth/default.nix"
  eq "default position" "SELECT position FROM meta WHERE attribute = 'hello'" "pkgs/hello/default.nix"
  eq "absolute positions" "SELECT count(*) FROM meta WHERE position LIKE '/%' OR position LIKE '%:%'" 0
  eq "list homepage" "SELECT homepage FROM meta WHERE attribute = 'list-homepage'" "https://first.example"
  eq "string homepage" "SELECT homepage FROM meta WHERE attribute = 'hello'" "https://example.org/hello"
  eq "broken" "SELECT group_concat(attribute) FROM meta WHERE broken = 1" "broken-thing"
  eq "insecure" "SELECT group_concat(attribute) FROM meta WHERE insecure = 1" "vulnerable"
  eq "license json" "SELECT json_extract(license, '\$.spdxId') FROM meta WHERE attribute = 'hello'" "MIT"
  eq "platforms trimmed" "SELECT count(*) FROM meta WHERE length(platforms) > 0" 0
  eq "maintainers json" "SELECT json_extract(maintainers, '\$[0].github') FROM meta WHERE attribute = 'hello'" "jane"
  eq "longdescription" "SELECT longdescription FROM meta WHERE attribute = 'hello'" "Says hello."
  eq "csv quoting" "SELECT description FROM meta WHERE attribute = 'awkward'" "multi
  line, \"quoted\" and 'single'"
  eq "empty license" "SELECT license FROM meta WHERE attribute = 'minimal'" ""
  eq "info source" "SELECT value FROM info WHERE key = 'nixpkgs-source'" "${./.}"
  eq "info system" "SELECT value FROM info WHERE key = 'system'" "${pkgdb.system}"

  touch $out
''
