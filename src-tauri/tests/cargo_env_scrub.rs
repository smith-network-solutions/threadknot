//! `scrub_cargo_env` has to be checked against a real child process, and from a
//! test binary of its own, because neither shortcut proves the thing that
//! matters.
//!
//! `/proc/<pid>/environ` is not a witness: it is the environment block handed to
//! `execve`, and glibc's `unsetenv` leaves it untouched, so a process that has
//! genuinely scrubbed itself still reads back dirty there. What a spawned agent
//! CLI or PTY shell actually inherits is libc's live `environ` — which is what
//! `Command` copies — so the assertion has to come from a child.
//!
//! One test, in its own binary, deliberately: the function mutates
//! process-global state, so anything running beside it is a data race on the
//! environment.

/// A child must not inherit cargo's build variables — and an installed build,
/// which cargo never launched, must be left alone.
///
/// `cargo test` is itself run by cargo, so this process starts with the exact
/// leak the function exists to remove; no fixture is needed for the CARGO_*
/// half.
#[test]
#[cfg(unix)]
fn scrubs_cargos_environment_from_children_but_only_under_cargo() {
    // Pre-condition. If cargo ever stops exporting this, the test is measuring
    // nothing and should say so rather than pass vacuously.
    let manifest_dir = std::env::var_os("CARGO_MANIFEST_DIR")
        .expect("expected `cargo test` to export CARGO_MANIFEST_DIR");

    // --- The gate: without cargo's marker, nothing is touched. ------------
    // `DEBUG` and `PROFILE` are ordinary names that belong to whoever set them
    // once cargo is not in the picture, so clearing them in an installed build
    // would be a bug.
    std::env::remove_var("CARGO_MANIFEST_DIR");
    std::env::set_var("DEBUG", "user-set");
    threadknot_lib::scrub_cargo_env();
    assert_eq!(
        std::env::var("DEBUG").ok().as_deref(),
        Some("user-set"),
        "scrubbed a user's DEBUG in an environment cargo did not create"
    );

    // --- Under cargo: everything injected goes, and a child sees that. ----
    std::env::set_var("CARGO_MANIFEST_DIR", &manifest_dir);
    // The unprefixed names cargo sets for build scripts but not for a test
    // binary — planted so the second half of the scrub is exercised too.
    std::env::set_var("OUT_DIR", "/fake/out");
    std::env::set_var("DEBUG", "true");

    threadknot_lib::scrub_cargo_env();

    let output = std::process::Command::new("env")
        .output()
        .expect("failed to run `env`");
    let child_env = String::from_utf8_lossy(&output.stdout);

    let leaked: Vec<&str> = child_env
        .lines()
        .filter(|line| {
            let name = line.split('=').next().unwrap_or_default();
            match name {
                // Kept on purpose: nothing fingerprints it, and clearing a
                // non-default one points a child's cargo at the wrong registry.
                "CARGO_HOME" => false,
                _ => {
                    name.starts_with("CARGO")
                        || matches!(name, "OUT_DIR" | "DEBUG" | "PROFILE" | "RUSTUP_TOOLCHAIN")
                }
            }
        })
        .collect();

    assert!(
        leaked.is_empty(),
        "child inherited cargo's build environment: {leaked:?}"
    );

    // And the scrub is surgical — unrelated variables survive.
    assert!(
        child_env.lines().any(|line| line.starts_with("PATH=")),
        "scrub removed PATH"
    );
}
