# Minimal nixpkgs-shaped tree for the pkgdb check.
{
  system ? builtins.currentSystem,
  config ? { },
  ...
}:
let
  here = toString ./.;
  mk =
    {
      name,
      file ? "pkgs/${name}/default.nix",
      meta ? { },
    }:
    # Fake derivation: the sandbox cannot write a real .drv, and nix-env -qa does not need one.
    {
      type = "derivation";
      name = "${name}-1.0";
      inherit system;
      outputName = "out";
      outputs = [ "out" ];
      outPath = "/nonexistent/${name}-1.0";
      drvPath = "/nonexistent/${name}-1.0.drv";
      pname = name;
      version = "1.0";
      meta = {
        position = "${here}/${file}:42";
        unfree = false;
        broken = false;
        description = "The ${name} package";
      } // meta;
    };
in
{
  hello = mk {
    name = "hello";
    meta = {
      longDescription = "Says hello.";
      homepage = "https://example.org/hello";
      license = {
        spdxId = "MIT";
        fullName = "MIT License";
        free = true;
      };
      platforms = [ "x86_64-linux" "aarch64-linux" ];
      maintainers = [ { name = "Jane"; github = "jane"; } ];
    };
  };
  unfree-thing = mk { name = "unfree-thing"; meta.unfree = true; };
  broken-thing = mk { name = "broken-thing"; meta.broken = true; };
  list-homepage = mk {
    name = "list-homepage";
    meta.homepage = [ "https://first.example" "https://second.example" ];
  };
  vulnerable = mk { name = "vulnerable"; meta.knownVulnerabilities = [ "CVE-2000-0001" ]; };
  # Quoting, commas and newlines must survive the CSV step.
  awkward = mk {
    name = "awkward";
    meta.description = "multi\nline, \"quoted\" and 'single'";
  };
  # Only description set: everything else must still come out as non-NULL strings.
  minimal = mk { name = "minimal"; meta = { }; };

  audioPkgs = {
    recurseForDerivations = true;
    synth = mk {
      name = "synth";
      file = "pkgs/applications/audio/synth/default.nix";
    };
  };
  # No recurseForDerivations: must not be indexed.
  hidden = {
    inner = mk { name = "inner"; };
  };
}
