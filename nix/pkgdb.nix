# Package index in nix-data's SQLite schema, built from one nixpkgs evaluation.
# `path` is the nixpkgs to index; callPackage defaults it to the consumer's pkgs.path.
{
  stdenv,
  runCommand,
  nix,
  jq,
  sqlite,
  path,
  system ? stdenv.hostPlatform.system,
}:
runCommand "nixpkgs-pkgdb"
  {
    passthru = { inherit system; };
    nativeBuildInputs = [
      nix
      jq
      sqlite
    ];
    # nix-env evaluates the whole package set; Nix's GC heap growth dominates its time.
    GC_INITIAL_HEAP_SIZE = "4294967296";
  }
  ''
    export HOME=$TMPDIR NIX_STATE_DIR=$TMPDIR/state NIX_PATH= NIX_CONF_DIR=$TMPDIR/etc
    mkdir -p "$NIX_STATE_DIR" "$NIX_CONF_DIR"

    # Unfree/broken/insecure must still evaluate; they are filtered or flagged below.
    nix-env -f ${path} -qa --json --meta \
      --arg config 'import ${path}/pkgs/top-level/packages-config.nix // { allowUnfree = true; allowBroken = true; allowInsecure = true; }' \
      --argstr system ${system} > raw.json

    # One flat object per free package. Text columns are never null, the app decodes them as String.
    jq -c --arg prefix '${path}/' '
      def str: if . == null then "" elif type == "string" then . else tojson end;
      def js:  if . == null then "" else tojson end;
      to_entries[]
      | select((.value.meta.unfree // false) != true)
      | .key as $attr | .value as $p | ($p.meta // {}) as $m
      | {
          attribute: $attr,
          pname: ($p.pname // $p.name | str),
          version: ($p.version | str),
          system: ($p.system | str),
          description: ($m.description | str),
          longdescription: ($m.longDescription | str),
          homepage: ($m.homepage | if type == "array" then (.[0] | str) else str end),
          license: ($m.license | js),
          platforms: ($m.platforms | js),
          maintainers: ($m.maintainers | js),
          position: ($m.position | str | ltrimstr($prefix) | sub(":[0-9]+$"; "")),
          broken: (if $m.broken == true then 1 else 0 end),
          insecure: (if $m.insecure == true or (($m.knownVulnerabilities // []) | length) > 0 then 1 else 0 end)
        }
    ' raw.json > flat.ndjson

    jq -r '[.attribute, .pname, .version, .system] | @csv' flat.ndjson > pkgs.csv
    jq -r '[.attribute, .description, .longdescription, .homepage, .license, .platforms,
            .maintainers, .position, .broken, .insecure] | @csv' flat.ndjson > meta.csv

    mkdir -p $out
    db=$out/nixpkgs.db
    sqlite3 "$db" <<'SQL'
    PRAGMA journal_mode = OFF;
    PRAGMA synchronous = OFF;
    CREATE TABLE pkgs (
      attribute TEXT NOT NULL PRIMARY KEY,
      pname TEXT NOT NULL DEFAULT ''',
      version TEXT NOT NULL DEFAULT ''',
      system TEXT NOT NULL DEFAULT '''
    );
    CREATE TABLE meta (
      attribute TEXT NOT NULL PRIMARY KEY,
      description TEXT NOT NULL DEFAULT ''',
      longdescription TEXT NOT NULL DEFAULT ''',
      homepage TEXT NOT NULL DEFAULT ''',
      license TEXT NOT NULL DEFAULT ''',
      platforms TEXT NOT NULL DEFAULT ''',
      maintainers TEXT NOT NULL DEFAULT ''',
      position TEXT NOT NULL DEFAULT ''',
      broken INTEGER NOT NULL DEFAULT 0,
      insecure INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE info (key TEXT PRIMARY KEY, value TEXT);
    CREATE INDEX pkgs_pname ON pkgs (pname);
    BEGIN;
    .import --csv pkgs.csv pkgs
    .import --csv meta.csv meta
    INSERT INTO info VALUES ('nixpkgs-source', '${path}'), ('system', '${system}');
    COMMIT;
    SQL
    sqlite3 "$db" VACUUM
  ''
