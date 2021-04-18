use vergen::{vergen, Config, ShaKind};

fn main() {
    let mut config = Config::default();

    *config.cargo_mut().features_mut() = false;
    *config.cargo_mut().profile_mut() = false;

    *config.git_mut().semver_mut() = false;
    *config.git_mut().sha_kind_mut() = ShaKind::Short;

    vergen(config).expect("Unable to generate the cargo keys!");
}
