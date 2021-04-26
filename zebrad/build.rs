use vergen::{vergen, Config, SemverKind, ShaKind};

/// Disable vergen env vars that could cause spurious reproducible build
/// failures
fn disable_non_reproducible(_config: &mut Config) {
    /*
    Currently, these features are disabled in `Cargo.toml`

    // We don't use build or host-specific env vars, because they can break
    // reproducible builds.
    *config.build_mut().enabled_mut() = false;
    *config.rustc mut().host_triple_mut() = false;

    // It's ok for reproducible builds to depend on the build OS. But most other
    // sysinfo should not change reproducible builds, so we disable it.
    *config.sysinfo mut().user_mut() = false;
    *config.sysinfo mut().memory_mut() = false;
    *config.sysinfo mut().cpu_vendor_mut() = false;
    *config.sysinfo mut().cpu_core_count_mut() = false;
    *config.sysinfo mut().cpu_name_mut() = false;
    *config.sysinfo mut().cpu_brand_mut() = false;
    *config.sysinfo mut().cpu_frequency_mut() = false;
     */
}

fn main() {
    let mut config = Config::default();
    disable_non_reproducible(&mut config);

    *config.git_mut().sha_kind_mut() = ShaKind::Short;
    *config.git_mut().semver_kind_mut() = SemverKind::Lightweight;

    // Disable env vars we aren't using right now
    *config.cargo_mut().features_mut() = false;

    // Disable git if we're building with an invalid `zebra/.git`
    match vergen(config) {
        Ok(_) => {}
        Err(e) => {
            eprintln!(
                "git error in vergen build script: skipping git env vars: {:?}",
                e
            );
            *config.git_mut().enabled_mut() = false;
            vergen(config).expect("non-git vergen should succeed");
        }
    }
}
