//! Argument lists for `pkexec nsc-helper`, shared by the system package operations.

/// `--flake <ref>` split on spaces, for the rebuild command after `--`.
pub fn rebuild_args(flake: Option<&str>) -> Vec<String> {
    match flake {
        Some(f) => ["--flake"].into_iter().chain(f.split(' ').filter(|a| !a.is_empty())).map(String::from).collect(),
        None => vec![],
    }
}

/// One `nsc-helper` call: `<mode> [--rebuild] [--flakepath P] [--update] --generations N
/// [--output FILE] -- [switch|…] [--impure] <rebuild args>`.
#[derive(Debug, Default, Clone)]
pub struct Call<'a> {
    /// `config`, `channel` or `flake`.
    pub mode: &'a str,
    pub rebuild: bool,
    pub flakepath: Option<&'a str>,
    pub update: bool,
    pub generations: u32,
    /// Where the helper writes the new configuration it reads from stdin.
    pub output: Option<&'a str>,
    pub impure: bool,
    pub rebuild_args: Vec<String>,
}

impl Call<'_> {
    pub fn args(&self) -> Vec<String> {
        let mut a = vec![self.mode.to_string()];
        if self.rebuild {
            a.push("--rebuild".into());
        }
        if let Some(p) = self.flakepath {
            a.extend(["--flakepath".into(), p.into()]);
        }
        if self.update {
            a.push("--update".into());
        }
        a.extend(["--generations".into(), self.generations.to_string()]);
        if let Some(o) = self.output {
            a.extend(["--output".into(), o.into()]);
        }
        a.extend(["--".into(), "switch".into()]);
        if self.impure {
            a.push("--impure".into());
        }
        a.extend(self.rebuild_args.iter().cloned());
        a
    }
}
