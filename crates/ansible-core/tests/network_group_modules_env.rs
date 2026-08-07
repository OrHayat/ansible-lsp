//! `ANSIBLE_NETWORK_GROUP_MODULES` beats the `ansible.cfg` key, which beats the shipped
//! default (T-072).
//!
//! Its own integration test, and deliberately the only test in this binary: setting a
//! process-wide variable from one of several threads would leak into whatever else was
//! reading config at that moment. Cargo gives each integration file its own process, so
//! here there is nothing else to leak into.

use ansible_core::config::AnsibleConfig;
use ansible_core::testing::CfgFs;
use std::path::Path;

#[test]
fn the_env_var_overrides_both_the_cfg_key_and_the_default() {
    let cfg = || {
        AnsibleConfig::load_in(
            Path::new("/p"),
            &CfgFs::some("[defaults]\nnetwork_group_modules = ios\n"),
        )
    };

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
    // skip every layer above the default. `CfgFs::none` is what `NoFile` used to be.
    let c = AnsibleConfig::load_in(Path::new("/p"), &CfgFs::none());
    assert!(c.is_network_platform("junos"), "env honoured without an ansible.cfg");
    assert!(!c.is_network_platform("ios"));

    // SAFETY: as above.
    unsafe { std::env::remove_var("ANSIBLE_NETWORK_GROUP_MODULES") };
    assert!(cfg().is_network_platform("ios"), "back to the cfg key");
}
