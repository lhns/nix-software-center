{
  pkgs ? import <nixpkgs> { },
  lib ? import <nixpkgs/lib>,
  nixos-appstream-data ? pkgs.callPackage ./nix/appstream.nix { },
  # Package index (nix/pkgdb.nix); when set, the app uses it via NSC_PKGDB.
  pkgdb ? null,
  # URL the app checks before installs; "" disables the check (NSC_ONLINE_CHECK_URL).
  onlineCheckUrl ? null,
}:
pkgs.stdenv.mkDerivation {
  pname = "nix-software-center";
  version = "0.1.2";

  src = [ ./. ];

  cargoDeps = pkgs.rustPlatform.importCargoLock {
    lockFile = ./Cargo.lock;
    outputHashes = {
      "nix-data-0.0.2" = "sha256-yts2bkp9cn4SuYPYjgTNbOwTtpFxps3TU8zmS/ftN/Q=";
    };
  };

  nativeBuildInputs =
    with pkgs;
    [
      appstream-glib
      polkit
      gettext
      desktop-file-utils
      meson
      ninja
      pkg-config
      git
      wrapGAppsHook4
    ]
    ++ (with pkgs.rustPlatform; [
      cargoSetupHook
      cargo
      rustc
    ]);

  buildInputs = with pkgs; [
    gdk-pixbuf
    glib
    gtk4
    gtksourceview5
    libadwaita
    libxml2
    openssl
    wayland
    adwaita-icon-theme
    desktop-file-utils
    nixos-appstream-data
  ];

  patchPhase = ''
    substituteInPlace ./src/lib.rs \
        --replace "/usr/share/app-info" "${nixos-appstream-data}/share/app-info"
  '';

  # Headless backend smoke test, for integration tests of the image the app ships in.
  postBuild = ''
    cargo build --release --offline --locked -p nsc-backend --features smoke --manifest-path ../Cargo.toml --target-dir src
  '';

  # gappsWrapperArgs also reach libexec, so the smoke binary sees the same index.
  preFixup =
    lib.optionalString (pkgdb != null) ''
      gappsWrapperArgs+=(--set-default NSC_PKGDB ${pkgdb}/nixpkgs.db)
    ''
    + lib.optionalString (onlineCheckUrl != null) ''
      gappsWrapperArgs+=(--set-default NSC_ONLINE_CHECK_URL ${lib.escapeShellArg onlineCheckUrl})
    '';

  postInstall = ''
    install -Dm755 src/release/nsc-backend-smoke $out/libexec/nix-software-center/nsc-backend-smoke
    wrapProgram $out/bin/nix-software-center --prefix PATH : '${
      lib.makeBinPath [
        pkgs.gnome-console
        pkgs.gtk3 # provides gtk-launch
        pkgs.sqlite
      ]
    }'
  '';
}
