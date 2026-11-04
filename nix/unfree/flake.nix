# Installs unfree apps from the registry's nixpkgs, with allowUnfree set: no --impure needed.
# Deliberately no committed lock, so it follows the registry pin of the machine it runs on.
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
