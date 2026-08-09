//! `EnvMap::from_process` is the one place the crate reads the real environment; every
//! precedence rule is unit-tested in `config.rs` against literal maps. This binary only has
//! to prove the pipe exists: a variable set in the process reaches `AnsibleConfig::load`.
//!
//! Its own integration test, and deliberately the only test in this binary: setting a
//! process-wide variable from one of several threads would leak into whatever else is
//! reading the environment at that moment. Cargo gives each integration file its own
//! process, so here there is nothing to leak into.

use ansible_core::config::AnsibleConfig;
use ansible_core::testing;

#[test]
fn a_process_env_var_reaches_the_loaded_config() {
    // SAFETY: single-threaded — this binary holds exactly one test. Cleared rather than
    // guarded-and-skipped: the test must still run on machines where these are exported.
    for var in [
        "ANSIBLE_CONFIG",
        "ANSIBLE_ROLES_PATH",
        "ANSIBLE_COLLECTIONS_PATH",
        "ANSIBLE_LIBRARY",
        "ANSIBLE_ACTION_PLUGINS",
        "ANSIBLE_HOME",
    ] {
        unsafe { std::env::remove_var(var) };
    }
    let root = testing::project(
        "env-snapshot-project",
        "[defaults]\nroles_path = ./roles\n",
        &[("roles/.keep", "")],
    );
    let other =
        testing::tree("env-snapshot-other", &[("team.cfg", "[defaults]\nroles_path = ./shared\n")]);

    assert_eq!(AnsibleConfig::load(&root).roles_path, Some(vec![root.join("roles")]));

    // SAFETY: single-threaded — this binary holds exactly one test.
    unsafe { std::env::set_var("ANSIBLE_CONFIG", other.join("team.cfg")) };
    assert_eq!(AnsibleConfig::load(&root).roles_path, Some(vec![other.join("shared")]));

    // SAFETY: as above.
    unsafe { std::env::remove_var("ANSIBLE_CONFIG") };
    assert_eq!(AnsibleConfig::load(&root).roles_path, Some(vec![root.join("roles")]));
}
