use nsc_backend::helper::{rebuild_args, Call};

fn v(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

#[test]
fn rebuild_args_split_the_flake() {
    assert_eq!(rebuild_args(Some("/etc/nixos#host  --x")), v(&["--flake", "/etc/nixos#host", "--x"]));
    assert!(rebuild_args(None).is_empty());
}

#[test]
fn each_variant_keeps_its_argument_order() {
    let rb = v(&["--flake", "f#h"]);
    let base = Call { generations: 3, rebuild_args: rb.clone(), ..Default::default() };
    let config = Call { mode: "config", output: Some("/c.nix"), impure: true, ..base.clone() };
    assert_eq!(
        config.args(),
        v(&["config", "--generations", "3", "--output", "/c.nix", "--", "switch", "--impure", "--flake", "f#h"])
    );
    let channel = Call { mode: "channel", rebuild: true, ..base.clone() };
    assert_eq!(
        channel.args(),
        v(&["channel", "--rebuild", "--generations", "3", "--", "switch", "--flake", "f#h"])
    );
    let channel_rm = Call { update: true, output: Some("/c.nix"), ..channel };
    assert_eq!(
        channel_rm.args(),
        v(&["channel", "--rebuild", "--update", "--generations", "3", "--output", "/c.nix", "--", "switch", "--flake", "f#h"])
    );
    let flake = Call { mode: "flake", rebuild: true, flakepath: Some("/f"), output: Some("/c.nix"), impure: true, ..base };
    assert_eq!(
        flake.args(),
        v(&["flake", "--rebuild", "--flakepath", "/f", "--generations", "3", "--output", "/c.nix", "--", "switch", "--impure", "--flake", "f#h"])
    );
}
