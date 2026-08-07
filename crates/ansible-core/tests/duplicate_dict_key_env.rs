//! `ANSIBLE_DUPLICATE_YAML_DICT_KEY` beats the `duplicate_dict_key` ini key, which beats the
//! shipped `warn` (T-102). Note the two spellings: the env var carries `YAML`, the ini key
//! does not.
//!
//! Live-verified against ansible-core 2.21.2 — cfg `error` + env `ignore` runs silently,
//! cfg `ignore` + env `error` refuses to load the file, and the env var applies with no
//! `ansible.cfg` present at all.
//!
//! Its own integration test, and deliberately the only test in this binary, for the reason
//! given in `network_group_modules_env.rs`: a process-wide variable set from one of several
//! threads leaks into whatever else is reading config at that moment.

use ansible_core::config::{AnsibleConfig, DuplicateDictKey};
use ansible_core::testing::CfgFs;
use std::path::Path;

fn with(cfg: Option<&str>) -> DuplicateDictKey {
    let fs = cfg.map(CfgFs::some).unwrap_or_else(CfgFs::none);
    AnsibleConfig::load_in(Path::new("/p"), &fs).duplicate_dict_key
}

#[test]
fn the_env_var_overrides_both_the_cfg_key_and_the_default() {
    const ERROR_CFG: &str = "[defaults]\nduplicate_dict_key = error\n";
    const IGNORE_CFG: &str = "[defaults]\nduplicate_dict_key = ignore\n";

    // Baseline: the cfg key is in force, and its absence means the shipped default.
    assert_eq!(with(Some(ERROR_CFG)), DuplicateDictKey::Error);
    assert_eq!(with(None), DuplicateDictKey::Warn);

    // SAFETY: single-threaded — this binary holds exactly one test.
    unsafe { std::env::set_var("ANSIBLE_DUPLICATE_YAML_DICT_KEY", "ignore") };
    assert_eq!(with(Some(ERROR_CFG)), DuplicateDictKey::Ignore, "env beats the cfg key");
    assert_eq!(with(None), DuplicateDictKey::Ignore, "env applies with no ansible.cfg");

    // And in the other direction, so this can't pass by always preferring the quieter value.
    unsafe { std::env::set_var("ANSIBLE_DUPLICATE_YAML_DICT_KEY", "error") };
    assert_eq!(with(Some(IGNORE_CFG)), DuplicateDictKey::Error);

    // A value Ansible rejects leaves the layer below intact rather than aborting, which is
    // where we knowingly diverge: Ansible refuses to run at all.
    unsafe { std::env::set_var("ANSIBLE_DUPLICATE_YAML_DICT_KEY", "False") };
    assert_eq!(with(Some(IGNORE_CFG)), DuplicateDictKey::Ignore, "cfg key survives");
    assert_eq!(with(None), DuplicateDictKey::Warn, "and the default survives");

    // SAFETY: as above.
    unsafe { std::env::remove_var("ANSIBLE_DUPLICATE_YAML_DICT_KEY") };
    assert_eq!(with(Some(ERROR_CFG)), DuplicateDictKey::Error, "back to the cfg key");
}
