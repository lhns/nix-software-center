{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, utils, ... }:
    utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
        };
      in
      rec
      {
        packages = rec {
          appstream-data = pkgs.callPackage ./nix/appstream.nix { };
          # Full index for this system; not part of the default package so CI and builds stay cheap.
          pkgdb = pkgs.callPackage ./nix/pkgdb.nix { inherit system; };
          # The fixture index of the pkgdb-small check, for the UI screenshot run in CI.
          pkgdb-small = pkgs.callPackage ./nix/pkgdb.nix { path = ./nix/tests/pkgdb-small; inherit system; };
          nix-software-center = pkgs.callPackage ./default.nix { nixos-appstream-data = appstream-data; };
          # As the desktops ship it: own index, no online check.
          nix-software-center-full = pkgs.callPackage ./default.nix {
            nixos-appstream-data = appstream-data;
            inherit pkgdb;
            onlineCheckUrl = "";
          };
          default = nix-software-center;
        };

        checks = {
          inherit (self.packages.${system}) nix-software-center;
          pkgdb-small = pkgs.callPackage ./nix/tests/pkgdb-small/check.nix { inherit system; };
        };
        hydraJobs = self.checks.${system};

        devShell = pkgs.mkShell {
          buildInputs = with pkgs; [
            cargo
            clippy
            desktop-file-utils
            rust-analyzer
            rustc
            rustfmt
            cairo
            gdk-pixbuf
            gobject-introspection
            graphene
            gtk4
            libadwaita
            libxml2
            meson
            ninja
            openssl
            pandoc
            pango
            pkg-config
            polkit
            sqlite
            wrapGAppsHook4
            self.packages.${system}.appstream-data
          ];
          RUST_SRC_PATH = "${pkgs.rust.packages.stable.rustPlatform.rustLibSrc}";
        };
      })
    // {
      overlays = {
        pkgs = final: prev: {
          nix-software-center = final.callPackage ./default.nix { };
          # system is passed explicitly: callPackage would otherwise fill it from the deprecated pkgs.system.
          nix-software-center-pkgdb = final.callPackage ./nix/pkgdb.nix {
            system = final.stdenv.hostPlatform.system;
          };
        };
        nixSoftwareCenterPkgs = final: prev: {
          nixSoftwareCenterPkgs = self.packages.${prev.system};
        };
        default = self.overlays.nixSoftwareCenterPkgs;
      };
      overlay = self.overlays.pkgs;
    };
}
