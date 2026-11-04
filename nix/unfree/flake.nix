# Installs unfree apps from the registry's nixpkgs, with allowUnfree set: no --impure needed.
# Locked: an unlocked flake makes `nix profile add` fail (it cannot write the lock). Bump with `nix flake update`.
{
  inputs.nixpkgs.url = "nixpkgs";

  outputs =
    { nixpkgs, ... }:
    {
      legacyPackages = nixpkgs.lib.genAttrs [ "x86_64-linux" "aarch64-linux" ] (
        system: import nixpkgs { inherit system; config.allowUnfree = true; }
      );
    };
}
