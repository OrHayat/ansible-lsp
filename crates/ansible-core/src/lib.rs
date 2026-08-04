pub mod ast;
pub mod cache;
pub mod condition;
pub mod config;
pub mod glob;
pub mod guard;
pub mod include_vars;
pub mod install;
pub mod keywords;
pub mod mutation;
pub mod parse;
pub mod parse_libyaml;
pub mod references;
pub mod resolve;
pub mod splitter;
pub mod vars;
pub mod workspace;

/// A path rendered the way Ansible would print it: POSIX separators.
///
/// Every path Ansible names — in an error message, in a playbook — comes from a control node,
/// which is POSIX-only. `PathBuf::join` gives `\` on Windows, so a path we computed and then
/// show to a user is a path no Ansible ever produced and no playbook ever wrote. Use this
/// wherever a path becomes *text*: error-message replicas, hover markdown, diagnostics.
/// Only rewritten on Windows, where `\` cannot occur in a filename — on Unix it legally can,
/// so the string passes through untouched.
pub fn posix_display(p: &std::path::Path) -> String {
    let s = p.to_string_lossy();
    if cfg!(windows) {
        s.replace('\\', "/")
    } else {
        s.into_owned()
    }
}
