//! `ANSIBLE_NETWORK_GROUP_MODULES` beats the `ansible.cfg` key, which beats the shipped
//! default (T-072).
//!
//! Its own integration test, and deliberately the only test in this binary: setting a
//! process-wide variable from one of several threads would leak into whatever else was
//! reading config at that moment. Cargo gives each integration file its own process, so
//! here there is nothing else to leak into.

use ansible_core::config::AnsibleConfig;
use ansible_core::fs::{Fs, Kind};
use std::path::{Path, PathBuf};

struct CfgFs(&'static str);

impl Fs for CfgFs {
    fn kind(&self, _p: &Path) -> Option<Kind> {
        Some(Kind::File)
    }
    fn read(&self, _p: &Path) -> Option<String> {
        Some(self.0.to_string())
    }
    fn read_dir(&self, _p: &Path) -> Vec<(PathBuf, Kind)> {
        Vec::new()
    }
    fn walk(&self, _root: &Path) -> Vec<(PathBuf, Vec<String>)> {
        Vec::new()
    }
    fn canonical(&self, p: &Path) -> Option<PathBuf> {
        Some(p.to_path_buf())
    }
}

#[test]
fn the_env_var_overrides_both_the_cfg_key_and_the_default() {
    let cfg = || AnsibleConfig::load_in(Path::new("/p"), &CfgFs("[defaults]\nnetwork_group_modules = ios\n"));

    // Baseline: the cfg key is in force.
    assert!(cfg().is_network_platform("ios"));
    assert!(!cfg().is_network_platform("junos"));

    // SAFETY: single-threaded — this binary holds exactly one test, which is why the
    // override lives here rather than beside the other config tests.
    unsafe { std::env::set_var("ANSIBLE_NETWORK_GROUP_MODULES", "junos, vyos") };
    let c = cfg();
    assert!(c.is_network_platform("junos"), "env value in force");
    assert!(c.is_network_platform("vyos"));
    assert!(!c.is_network_platform("ios"), "env replaces the cfg key wholesale");

    // It applies with no ansible.cfg at all — the early return for a missing file used to
    // skip every layer above the default.
    struct NoFile;
    impl Fs for NoFile {
        fn kind(&self, _p: &Path) -> Option<Kind> {
            None
        }
        fn read(&self, _p: &Path) -> Option<String> {
            None
        }
        fn read_dir(&self, _p: &Path) -> Vec<(PathBuf, Kind)> {
            Vec::new()
        }
        fn walk(&self, _root: &Path) -> Vec<(PathBuf, Vec<String>)> {
            Vec::new()
        }
        fn canonical(&self, _p: &Path) -> Option<PathBuf> {
            None
        }
    }
    let c = AnsibleConfig::load_in(Path::new("/p"), &NoFile);
    assert!(c.is_network_platform("junos"), "env honoured without an ansible.cfg");
    assert!(!c.is_network_platform("ios"));

    // SAFETY: as above.
    unsafe { std::env::remove_var("ANSIBLE_NETWORK_GROUP_MODULES") };
    assert!(cfg().is_network_platform("ios"), "back to the cfg key");
}
