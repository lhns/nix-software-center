# Integration spec: webtop-nix and kube-webtop-workspaces

How this fork ships in the per-user desktops (webtop-nix and kube-webtop-workspaces, kww).

## Contracts the fork provides

| what | where | notes |
| --- | --- | --- |
| app | `default.nix`, `{ pkgdb ? null, onlineCheckUrl ? null, nixos-appstream-data ? <snowfall, converted>, ... }` (more options below) | `final.callPackage`-able; builds against the caller's nixpkgs |
| package index | `nix/pkgdb.nix` → `$out/nixpkgs.db` | nix-data's SQLite schema plus `meta.unfree`; evaluated from the caller's `pkgs.path` for its own system; free and unfree packages both in it; `info` table holds `nixpkgs-source`, `system` and `nsc-catalogue-version` |
| `NSC_PKGDB` | env, set by the wrapper when `pkgdb` is given (`--set-default`) | replaces the nix-data-db download for both the package list and "latest versions" |
| `NSC_ONLINE_CHECK_URL` | env, set by the wrapper when `onlineCheckUrl` is given | URL the app GETs before installs/updates; empty disables the check |
| smoke binary | `$out/libexec/nix-software-center/nsc-backend-smoke` | headless; same backend functions as the UI; reads `NSC_PKGDB` |
| AppStream data | `nix/appstream.nix` | snowfallorg/nixos-appstream-data, free and unfree sections merged, 64/128 px icons |

The app only runs `nix profile list --json|add|remove|upgrade`. It knows nothing about
home-manager, profile hooks or kww; success is the exit code, the installed list is re-read
from `nix profile list --json` after each command, and progress comes from `json-log-path`
(a unix socket passed through `NIX_CONFIG`), so nested `nix` calls a hook makes show up too.

All work goes through one app-wide queue, shown in the Activity tab; the window stays usable. Profile
installs and removals are batched: one `nix profile` command runs at a time, all queued installs
become one `nix profile add a b c` (`install` on old Nix), queued removals one
`nix profile remove a b`, in queue order. Nix reverts a failed command as a whole, so a failed batch
of several is retried package by package. Updates, system and nix-env operations are exclusive: they
run alone and nothing queued after them starts first. Finished entries (with their log) stay in the
tab until removed. Cancel stops the running batch without retrying, so every package in it can be
retried; a queued entry is simply dropped. Removing asks for confirmation first. The smoke binary
accepts several installables (one `add`).

## webtop-nix

1. `pkgs/nix-software-center.nix`: `fetchFromGitHub` of `lhns/nix-software-center` at a pinned
   rev (as `pkgs/pcmflux.nix` pins its source), then
   ```nix
   callPackage "${src}/default.nix" {
     pkgdb = callPackage "${src}/nix/pkgdb.nix" { };
     # Nix reports a missing network itself, in the streamed output; the desktops' egress
     # policy then need not allow nmcheck.gnome.org.
     onlineCheckUrl = "";
   }
   ```
   `callPackage` from `final`, so the index is evaluated from the image's nixpkgs, which is what
   the flake registry's `nixpkgs` entry resolves to. Index and registry pin cannot drift.
2. `overlay.nix`: `nix-software-center = final.callPackage ./pkgs/nix-software-center.nix { };`
   and `nix-software-center-pkgdb` exposed separately (the index is its own derivation, so the
   image layering can place it with the other nixpkgs-bump paths).
3. Module option `services.webtop.softwareCenter.enable` (`mkEnableOption`, default `false`):
   ```nix
   environment.systemPackages = [ pkgs.nix-software-center ];
   # nix-data falls back to this when the user has no ~/.config/nix-data/config.json:
   # all fields unset, so no welcome dialog and no NixOS-config mode.
   environment.etc."nix-data/config.json".text = "{}";
   ```
4. CI: add `nix-software-center` and `nix-software-center-pkgdb` to the per-system packages
   and checks. Extend `checks.module-eval` with the option enabled. Optional: in `tests/vm.nix`,
   start the app in the session and assert it is still running after 20 s and its log has no
   `LoadError`.

Cost, measured in the fork's CI (`pkgdb` job, nixpkgs `3ec6363`): about 140,000 packages per
system, built in 75–85 s per system per nixpkgs bump. `packages.nix-software-center-full` is the package as the desktops ship it; CI starts it on headless weston with that index on both arches. The index is 227 MB raw, 12.3 MB with
`zstd -19` (what an image layer costs to pull) and 18.4 MB with `gzip -9`. `platforms` takes about
half of the raw size and `license` a tenth. Storing only "available on this system" instead of the
platform list would halve the raw size; that is a separate decision, because the details page
shows the list. AppStream data: 25.6 MB, 1,450 apps.

## kube-webtop-workspaces

1. Bump the `webtop-nix` input; set `services.webtop.softwareCenter.enable = true;` in
   `image/nixos/desktop.nix` (base image, on by default).
2. Image layering: put `nix-software-center-pkgdb` in the group that changes with nixpkgs.
3. Boot test (`.github/scripts/boot-test.sh`), both arches, after the existing `nix profile`
   block (which already proves nixpkgs installs from the cache). The installable must not be in
   the profile yet:
   ```sh
   nsc=$(as_user sh -c 'readlink -f "$(command -v nix-software-center)"')
   smoke=$(dirname "$(dirname "$nsc")")/libexec/nix-software-center/nsc-backend-smoke
   as_user "$smoke" nixpkgs#cowsay --require-progress || fail "software center backend smoke failed"
   ```
   It lists, adds, finds the new element (by element-name diff), upgrades `--all`, removes it
   by name and checks it is gone, and asserts the progress socket saw at least one activity.
   Through kww's patched `nix profile` this exercises the declaration backend, the profile hook
   and the apply step end to end.
4. Declared packages: before the smoke, make sure the `ci` user has at least one declared element
   besides the one the smoke adds (e.g. `as_user nix profile add nixpkgs#tree`), so the listing
   parses declared elements (`storePaths: []`, `backend` fields) on a real desktop, and the smoke
   proves it leaves the others alone (check `tree` is still listed afterwards).
5. ADR 0013: amend the "No GUI for now" consequence with a dated line once it ships.
6. Unfree: "nixpkgs unfree" is a built-in source that installs from `NSC_UNFREE_FLAKE`
   (below). Set it to `nixpkgs-unfree`, a registry entry pointing at a flake with
   `config.allowUnfree`, and resolve `flake:nixpkgs-unfree` in `packages.json` restores the way
   `flake:nixpkgs` is, so restores survive image updates.

## Not covered

- Per-flake update checks for non-nixpkgs elements (Updates shows none; "Update all" runs
  `nix profile upgrade --all`).
- Dark mode follows the `org.freedesktop.appearance color-scheme` portal setting; verify on a
  desktop that a dark Plasma gives a dark app.

## Package sources and the catalogue format

Sources are flakes that publish packages plus a prebuilt catalogue; users manage them in
Preferences (Sources). Two are built in: "nixpkgs" and "nixpkgs unfree", both read from the baked-in
`nixpkgs.db`, so they work offline.

### Options (`default.nix`) and environment

| option | env (wrapper `--set-default`) | meaning |
| --- | --- | --- |
| `systemPackages ? true` | `NSC_SYSTEM_PACKAGES=0` | read once at startup. `false` forces no system packages, no welcome screen, no configuration/flake preferences and refuses system jobs; Preferences then holds only Sources and the menu entry reads "Sources" |
| `unfreeFlake ? null` | `NSC_UNFREE_FLAKE` | flake that installs unfree apps, as `<flake>#<attr>`. Default `github:lhns/nix-software-center?dir=nix/unfree` (its own locked `nixpkgs`, `allowUnfree`, no `--impure`; a deployer that wants the system's pin sets `NSC_UNFREE_FLAKE`, e.g. a registry flake). A bare registry name such as `nixpkgs-unfree` is recognised as `flake:nixpkgs-unfree` in `nix profile list` |
| `unfreeListedDefault ? null` | `NSC_UNFREE_LISTED_DEFAULT` (`0`/`1`) | whether "nixpkgs unfree" is listed until the user toggles it; default on. Hiding only hides: installed apps keep their label and updates |
| none (not a package option) | `NSC_SYSTEM_MANIFEST` | read-only JSON file listing the apps built into the image: `{"packages": [{"pname": "dolphin", "version": "24.02", "path": "/nix/store/…"}]}`. Those apps show "Included with desktop" (with Open if launchable) instead of Install/Remove on the package page and as a tile badge. Read once at startup, so an image update that changes the file takes effect on restart. Matching is by `pname` and `version` against the built-in catalogue (the catalogue holds no store paths, so `path` is informational): attributes with the manifest's version win; if none has it, a lone candidate still matches (a patched package) and several match nothing. Then top-level attributes win over set members (`git` over `haskellPackages.git`), then those named like the `pname`. A third-party source's `<id>:<attr>` never matches. The user's own install (profile or `packages.json`) wins: it shows Installed with Remove. Unset: off. Set but unreadable, malformed or over 16 MiB: off, with a warning in the log. The wrapper does not set it (`default.nix` has no argument for it); the deployer exports it in the session |

User sources and explicit toggles live in `~/.config/nix-software-center/sources.json`; an untouched
built-in follows the deployer default. A file that does not parse is kept as `sources.json.bad` and
defaults are used until the user changes something.

### Catalogue (for source authors)

The flake outputs `nscCatalogue.<system>`, a directory with `catalogue.db` (SQLite) and optionally
`share/app-info/`. Make it a fixed-output fetch so clients only download; the app runs
`nix build <flake>#nscCatalogue.<system> --out-link ~/.cache/nix-software-center/catalogues/<id>` when the
source is added, on Refresh and when older than a day.

Tables (plain tables only; a view, trigger or virtual table makes the whole source be refused):

- `pkgs(attribute, pname, version, system)`
- `meta(attribute, description, longdescription, homepage, license, platforms, maintainers, position, broken, insecure, unfree)`
  (`license` and `maintainers` are JSON text as in `nix/pkgdb.nix`; `unfree` may be missing, then 0)
- `info(key, value)` with `nsc-catalogue-version` = `1`; any other version is refused

`attribute` is an opaque id within the source, installed as `<flake>#<attribute>`, so it must be an
output of the flake's `legacyPackages.<system>` or `packages.<system>`. `attribute` and `pname`
must match `[A-Za-z0-9_+.-]+` (they reach shell strings); other rows are dropped and counted.
Text is shown as plain text, never as markup. Third-party apps are listed from the catalogue only
(no AppStream data yet), as `<sourceid>:<attribute>` in the app and with their source named.
A name that two sources share is listed twice, once per source.

Installing an unfree app asks for consent at the Install click. The installed element, whose
`originalUrl` names the unfree flake, is the acceptance: updates and restores do not ask.
Removing a source never uninstalls: its apps stay, keep updating from the flake and show
"source removed".
