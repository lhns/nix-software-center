{
  pkgs ? import <nixpkgs> { },
  lib ? import <nixpkgs/lib>,
  nixos-appstream-data ? pkgs.callPackage ./nix/appstream.nix { },
  # Package index (nix/pkgdb.nix); when set, the app uses it via NSC_PKGDB.
  pkgdb ? null,
  # URL the app checks before installs; "" disables the check (NSC_ONLINE_CHECK_URL).
  onlineCheckUrl ? null,
  # false: image-based desktop without NixOS configuration or system packages; the window then
  # holds only the package sources (NSC_SYSTEM_PACKAGES=0).
  systemPackages ? true,
  # Flake that installs unfree apps (NSC_UNFREE_FLAKE); null keeps the app's default.
  unfreeFlake ? null,
  # Whether "nixpkgs unfree" is listed until the user toggles it (NSC_UNFREE_LISTED_DEFAULT); null = yes.
  unfreeListedDefault ? null,
  # Adds the screenshot hooks of src/ui/uitest.rs; only for the CI UI run.
  uitest ? false,
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
    libadwaita
    libxml2
    openssl
    wayland
    adwaita-icon-theme
    desktop-file-utils
    nixos-appstream-data
  ];

  mesonFlags = lib.optional uitest "-Duitest=true";

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
    ''
    + lib.optionalString (!systemPackages) ''
      gappsWrapperArgs+=(--set-default NSC_SYSTEM_PACKAGES 0)
    ''
    + lib.optionalString (unfreeFlake != null) ''
      gappsWrapperArgs+=(--set-default NSC_UNFREE_FLAKE ${lib.escapeShellArg unfreeFlake})
    ''
    + lib.optionalString (unfreeListedDefault != null) ''
      gappsWrapperArgs+=(--set-default NSC_UNFREE_LISTED_DEFAULT ${if unfreeListedDefault then "1" else "0"})
    '';

  postInstall = ''
    install -Dm755 src/release/nsc-backend-smoke $out/libexec/nix-software-center/nsc-backend-smoke
    wrapProgram $out/bin/nix-software-center --prefix PATH : '${
      lib.makeBinPath [
        pkgs.gtk3 # provides gtk-launch
        pkgs.sqlite
      ]
    }'
  '';
}
