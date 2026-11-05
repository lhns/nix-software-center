# AppStream catalog in the app-info layout nsc reads (one DEP-11 YAML, icons/nixos/<size>/).
# Conversion adapted from michaelmassoni/nix-software-center (GPL-3), `nixos-app-info`.
{
  lib,
  runCommand,
  fetchFromGitHub,
  appstream,
  gzip,
}:
let
  src = fetchFromGitHub {
    owner = "snowfallorg";
    repo = "nixos-appstream-data";
    rev = "df1a03d7d838026f3c70ca18b1d8867f605ed51c";
    hash = "sha256-uXNpJv687gJMITVYsfuGOywoElKxEcZMRd6HYVHFu24=";
  };
  # Free and unfree apps; the app lists them as two sources. The file name stays x86_64: the
  # content does not depend on the arch.
  sections = [
    "${src}/appstream/nixos-unstable"
    "${src}/appstream/nixos-unstable-unfree"
  ];
in
runCommand "nixos-appstream-data"
  {
    nativeBuildInputs = [
      appstream
      gzip
    ];
  }
  ''
    # One <components> document: DEP-11 YAML has a header document, so it cannot be concatenated.
    zcat ${lib.elemAt sections 0}/Components-x86_64-linux.xml.gz | sed -z 's#</components>[[:space:]]*$##' > nixos.xml
    zcat ${lib.elemAt sections 1}/Components-x86_64-linux.xml.gz | sed -z 's#^.*<components[^>]*>##' >> nixos.xml
    [ "$(grep -c '</components>' nixos.xml)" = 1 ] || { echo "merge of the two sections failed" >&2; exit 1; }
    appstreamcli convert nixos.xml nixos.yml
    mkdir -p $out/share/app-info/xmls
    gzip -9 -n -c nixos.yml > $out/share/app-info/xmls/nixos_x86_64_linux.yml.gz
    echo "apps: $(grep -c '^Package:' nixos.yml)"

    # Both sections ship the same icon names; the free section's tarball is extracted last.
    for size in 64x64 128x128; do
      dir=$out/share/app-info/icons/nixos/$size
      mkdir -p $dir
      for section in ${lib.concatStringsSep " " (lib.reverseList sections)}; do
        tar -xzf $section/icons-$size.tar.gz -C $dir
      done
    done
  ''
