# AppStream catalog in the app-info layout nsc reads (one DEP-11 YAML, icons/nixos/<size>/).
# Conversion adapted from michaelmassoni/nix-software-center (GPL-3), `nixos-app-info`.
{
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
  # Free apps only. The file name stays x86_64: the content does not depend on the arch.
  section = "${src}/appstream/nixos-unstable";
in
runCommand "nixos-appstream-data"
  {
    nativeBuildInputs = [
      appstream
      gzip
    ];
  }
  ''
    zcat ${section}/Components-x86_64-linux.xml.gz > nixos.xml
    appstreamcli convert nixos.xml nixos.yml
    mkdir -p $out/share/app-info/xmls
    gzip -9 -n -c nixos.yml > $out/share/app-info/xmls/nixos_x86_64_linux.yml.gz
    echo "apps: $(grep -c '^Package:' nixos.yml)"

    for size in 64x64 128x128; do
      dir=$out/share/app-info/icons/nixos/$size
      mkdir -p $dir
      tar -xzf ${section}/icons-$size.tar.gz -C $dir
    done
  ''
