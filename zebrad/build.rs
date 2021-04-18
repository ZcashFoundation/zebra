use vergen::{vergen, Config, ShaKind};

fn main() {
    let mut config = Config::default();
    *config.git_mut().branch_mut() = false;
    *config.git_mut().commit_timestamp_mut() = false;
    *config.git_mut().semver_mut() = false;
    *config.git_mut().sha_kind_mut() = ShaKind::Short;
    vergen(config).expect("Unable to generate the cargo keys!");
}
