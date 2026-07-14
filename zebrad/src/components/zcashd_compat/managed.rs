use std::{
    fs::{self, OpenOptions},
    io::{BufReader, Read, Write},
    path::{Path, PathBuf},
    thread::sleep,
    time::{Duration, Instant},
};

use color_eyre::eyre::{eyre, Report};
use flate2::read::GzDecoder;
use reqwest::{blocking::Client, redirect::Policy, Url};
use sha2::{Digest, Sha256};
use tar::Archive;
use tempfile::NamedTempFile;

use super::{
    Config, ConfigZcashdBinarySource, ZcashdReleaseManifest, EMBEDDED_ZCASHD_RELEASE_MANIFEST,
};
use crate::components::zcashd_compat::supervisor::is_command_resolvable;

/// Maximum time a managed archive download is allowed to take.
const MANAGED_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// Maximum time a process waits for another live process to finish a managed install.
const INSTALL_LOCK_WAIT_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// Stale age for legacy lock files that do not contain owner metadata.
const LEGACY_INSTALL_LOCK_STALE_AFTER: Duration = Duration::from_secs(30);
const INSTALL_LOCK_RETRY_DELAY: Duration = Duration::from_millis(250);

/// Effective `zcashd` source after local-path overrides are applied.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ZcashdBinarySource {
    /// Explicit local executable path.
    Path(PathBuf),
    /// Embedded release download and cache.
    Embedded,
}

/// Returns the current platform target triple used for managed release lookups.
pub fn zcashd_target_triple() -> Option<&'static str> {
    let target = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some("x86_64-pc-linux-gnu"),
        _ => None,
    }?;

    EMBEDDED_ZCASHD_RELEASE_MANIFEST
        .artifact_for_target(target)
        .map(|_| target)
}

/// Resolves the effective source for `zcashd`.
pub fn effective_zcashd_source(config: &Config) -> Result<ZcashdBinarySource, Report> {
    if let Some(path) = config.zcashd_path.clone() {
        return Ok(ZcashdBinarySource::Path(path));
    }

    match config.zcashd_source {
        ConfigZcashdBinarySource::Embedded => Ok(ZcashdBinarySource::Embedded),
        ConfigZcashdBinarySource::Path => Err(eyre!(
            "zcashd_compat.zcashd_source=path requires zcashd_compat.zcashd_path to be set"
        )),
    }
}

/// Resolves and validates the `zcashd` executable path.
pub fn resolve_zcashd_binary_path(
    config: &Config,
    state_cache_dir: &Path,
) -> Result<PathBuf, Report> {
    match effective_zcashd_source(config)? {
        ZcashdBinarySource::Path(path) => {
            if !is_command_resolvable(&path) {
                return Err(eyre!(
                    "zcashd-compat mode could not resolve zcashd_path={}",
                    path.display()
                ));
            }
            Ok(path)
        }
        ZcashdBinarySource::Embedded => resolve_managed_zcashd_binary(state_cache_dir),
    }
}

/// Resolves the managed zcashd binary from the embedded release manifest.
pub fn resolve_managed_zcashd_binary(state_cache_dir: &Path) -> Result<PathBuf, Report> {
    resolve_managed_zcashd_binary_from_manifest(&EMBEDDED_ZCASHD_RELEASE_MANIFEST, state_cache_dir)
}

/// Returns the managed zcashd binary cache path without creating directories,
/// or `None` when managed downloads are unsupported for this target.
#[allow(dead_code)]
pub(super) fn managed_zcashd_binary_path(state_cache_dir: &Path) -> Option<PathBuf> {
    let target = zcashd_target_triple()?;

    Some(
        managed_zcashd_cache_dir(
            state_cache_dir,
            &EMBEDDED_ZCASHD_RELEASE_MANIFEST.release_tag,
            target,
        )
        .join("zcashd"),
    )
}

/// Returns whether the cached managed zcashd binary is current for this target,
/// or `None` when managed downloads are unsupported for this target.
#[allow(dead_code)]
pub(super) fn cached_managed_zcashd_binary_is_current(
    state_cache_dir: &Path,
) -> Result<Option<bool>, Report> {
    let Some(target) = zcashd_target_triple() else {
        return Ok(None);
    };
    let artifact = EMBEDDED_ZCASHD_RELEASE_MANIFEST.artifact_for_target(target);
    let Some(artifact) = artifact else {
        return Ok(None);
    };

    let Some(binary_path) = managed_zcashd_binary_path(state_cache_dir) else {
        return Ok(None);
    };

    let provenance_path = binary_path.with_file_name("zcashd.sha256");

    Ok(Some(
        binary_path.is_file()
            && provenance_matches(&provenance_path, &artifact.runtime_archive_sha256)?,
    ))
}

fn resolve_managed_zcashd_binary_from_manifest(
    manifest: &ZcashdReleaseManifest,
    state_cache_dir: &Path,
) -> Result<PathBuf, Report> {
    let target = zcashd_target_triple().ok_or_else(|| {
        eyre!(
            "zcashd-compat managed downloads are unsupported for this platform ({}/{})",
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    })?;

    let artifact = manifest.artifact_for_target(target).ok_or_else(|| {
        eyre!(
            "no managed zcashd release is configured for target {target}; \
                 set zcashd_compat.zcashd_path to a local zcashd binary"
        )
    })?;

    let cache_dir = managed_zcashd_cache_dir(state_cache_dir, &manifest.release_tag, target);
    fs::create_dir_all(&cache_dir)?;

    let binary_path = cache_dir.join("zcashd");
    let provenance_path = cache_dir.join("zcashd.sha256");
    if binary_path.is_file()
        && provenance_matches(&provenance_path, &artifact.runtime_archive_sha256)?
    {
        return Ok(binary_path);
    }

    let _lock = acquire_lock(
        &cache_dir.join(".install.lock"),
        INSTALL_LOCK_WAIT_TIMEOUT,
        LEGACY_INSTALL_LOCK_STALE_AFTER,
    )?;

    // Re-check after acquiring the lock.
    if binary_path.is_file()
        && provenance_matches(&provenance_path, &artifact.runtime_archive_sha256)?
    {
        return Ok(binary_path);
    }

    let mut archive_temp = NamedTempFile::new_in(&cache_dir)?;
    download_archive(&artifact.runtime_archive_url, archive_temp.as_file_mut())?;
    archive_temp.as_file_mut().sync_all()?;

    let archive_sha256 = sha256_hex_file(archive_temp.path())?;
    if archive_sha256 != artifact.runtime_archive_sha256 {
        return Err(eyre!(
            "managed zcashd archive hash mismatch for target {target}: expected {}, got {archive_sha256}",
            artifact.runtime_archive_sha256
        ));
    }

    let extracted_temp = NamedTempFile::new_in(&cache_dir)?;
    extract_archive_member_to_path(
        archive_temp.path(),
        &artifact.runtime_archive_member_binary_path,
        extracted_temp.path(),
    )?;
    make_executable(extracted_temp.path())?;
    extracted_temp.persist(&binary_path).map_err(|err| {
        eyre!(
            "failed to persist managed zcashd binary {}: {}",
            binary_path.display(),
            err.error
        )
    })?;
    fs::write(
        &provenance_path,
        format!("{}\n", artifact.runtime_archive_sha256),
    )?;

    Ok(binary_path)
}

fn managed_zcashd_cache_dir(state_cache_dir: &Path, release_tag: &str, target: &str) -> PathBuf {
    state_cache_dir
        .join("zcashd-compat")
        .join("bin")
        .join(release_tag)
        .join(target)
}

/// Downloads one managed release archive to `out`.
///
/// Security constraints:
/// - production code requires HTTPS URLs only;
/// - redirects must remain HTTPS;
/// - tests may use localhost HTTP endpoints.
fn download_archive(url: &str, out: &mut fs::File) -> Result<(), Report> {
    let parsed =
        Url::parse(url).map_err(|err| eyre!("invalid managed zcashd URL '{url}': {err}"))?;
    if parsed.scheme() != "https" {
        #[cfg(test)]
        let localhost_http = parsed.scheme() == "http"
            && parsed
                .host_str()
                .map(|host| host == "127.0.0.1" || host == "localhost")
                .unwrap_or(false);
        #[cfg(not(test))]
        let localhost_http = false;

        if !localhost_http {
            return Err(eyre!("managed zcashd URL must use https: {url}"));
        }
    }

    let client = Client::builder()
        .redirect(Policy::limited(5))
        .timeout(MANAGED_DOWNLOAD_TIMEOUT)
        .build()
        .map_err(|err| eyre!("failed building managed zcashd HTTP client: {err}"))?;
    let mut response = client
        .get(parsed)
        .send()
        .map_err(|err| eyre!("failed downloading managed zcashd archive: {err}"))?;
    if !response.status().is_success() {
        return Err(eyre!(
            "managed zcashd download failed with HTTP status {}",
            response.status()
        ));
    }
    if response.url().scheme() != "https" {
        #[cfg(test)]
        let localhost_http = response.url().scheme() == "http"
            && response
                .url()
                .host_str()
                .map(|host| host == "127.0.0.1" || host == "localhost")
                .unwrap_or(false);
        #[cfg(not(test))]
        let localhost_http = false;

        if localhost_http {
            // allowed only in tests
        } else {
            return Err(eyre!(
                "managed zcashd download redirected to non-https URL: {}",
                response.url()
            ));
        }
    }

    std::io::copy(&mut response, out)
        .map_err(|err| eyre!("failed writing managed zcashd archive to cache: {err}"))?;
    Ok(())
}

/// Extracts a single archive member into `destination`.
///
/// This is used to pull only the configured `zcashd` binary from a release
/// archive, rather than unpacking all members.
fn extract_archive_member_to_path(
    archive_path: &Path,
    member_path: &str,
    destination: &Path,
) -> Result<(), Report> {
    let file = fs::File::open(archive_path)?;
    let reader = BufReader::new(file);
    let decoder = GzDecoder::new(reader);
    let mut archive = Archive::new(decoder);
    let requested = normalize_member_path(member_path);

    for entry in archive.entries()? {
        let mut entry = entry?;
        let entry_path = entry.path()?;
        let candidate = normalize_member_path(entry_path.to_string_lossy().as_ref());
        if candidate == requested {
            entry
                .unpack(destination)
                .map_err(|err| eyre!("failed extracting managed zcashd binary: {err}"))?;
            return Ok(());
        }
    }

    Err(eyre!(
        "managed zcashd archive does not contain expected member path '{}'",
        member_path
    ))
}

/// Normalizes archive entry paths for stable matching.
///
/// Tar archives can contain either `./bin/zcashd` or `bin/zcashd`; this helper
/// canonicalizes both into the same form.
fn normalize_member_path(path: &str) -> String {
    path.trim_start_matches("./").to_string()
}

/// Returns `true` if `path` exists and stores exactly `expected_sha256`.
fn provenance_matches(path: &Path, expected_sha256: &str) -> Result<bool, Report> {
    if !path.is_file() {
        return Ok(false);
    }

    let value = fs::read_to_string(path)
        .map(|content| content.trim().to_string())
        .unwrap_or_default();
    Ok(value == expected_sha256)
}

/// Computes the lowercase hex SHA256 digest for `path`.
fn sha256_hex_file(path: &Path) -> Result<String, Report> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 16 * 1024];

    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }

    let digest = hasher.finalize();
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// Makes `path` executable on Unix targets.
///
/// Non-Unix targets currently no-op because managed release targets are Linux.
fn make_executable(path: &Path) -> Result<(), Report> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)?;
    }

    Ok(())
}

struct InstallLock {
    path: PathBuf,
}

impl Drop for InstallLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Acquires an exclusive lock file, retrying until `timeout`.
///
/// This prevents concurrent zebrad processes from racing archive downloads and
/// replacing the same cached binary simultaneously.
fn acquire_lock(
    lock_path: &Path,
    wait_timeout: Duration,
    stale_after: Duration,
) -> Result<InstallLock, Report> {
    let started = Instant::now();
    loop {
        match OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(lock_path)
        {
            Ok(mut file) => {
                if let Err(error) = write_install_lock_owner(&mut file, lock_path) {
                    let _ = fs::remove_file(lock_path);
                    return Err(error);
                }

                return Ok(InstallLock {
                    path: lock_path.to_path_buf(),
                });
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                if remove_stale_lock(lock_path, stale_after)? {
                    continue;
                }

                if started.elapsed() >= wait_timeout {
                    return Err(eyre!(
                        "timed out after {} seconds waiting for managed zcashd installation lock: {}",
                        wait_timeout.as_secs(),
                        lock_path.display(),
                    ));
                }

                sleep(INSTALL_LOCK_RETRY_DELAY);
            }
            Err(err) => {
                return Err(eyre!(
                    "failed to create managed zcashd installation lock {}: {err}",
                    lock_path.display()
                ))
            }
        }
    }
}

fn write_install_lock_owner(file: &mut fs::File, lock_path: &Path) -> Result<(), Report> {
    writeln!(file, "pid={}", std::process::id()).map_err(|err| {
        eyre!(
            "failed to write managed zcashd installation lock {}: {err}",
            lock_path.display()
        )
    })?;
    file.sync_all().map_err(|err| {
        eyre!(
            "failed to sync managed zcashd installation lock {}: {err}",
            lock_path.display()
        )
    })
}

fn remove_stale_lock(lock_path: &Path, stale_after: Duration) -> Result<bool, Report> {
    let content = match fs::read_to_string(lock_path) {
        Ok(content) => content,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(err) => {
            return Err(eyre!(
                "failed to read managed zcashd installation lock {}: {err}",
                lock_path.display()
            ))
        }
    };

    if let Some(pid) = lock_owner_pid(&content) {
        if process_is_running(pid) {
            return Ok(false);
        }
    } else if !lock_file_is_older_than(lock_path, stale_after)? {
        return Ok(false);
    }

    match fs::remove_file(lock_path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(err) => Err(eyre!(
            "failed to remove stale managed zcashd installation lock {}: {err}",
            lock_path.display()
        )),
    }
}

fn lock_owner_pid(content: &str) -> Option<u32> {
    content.lines().find_map(|line| {
        line.strip_prefix("pid=")
            .and_then(|pid| pid.trim().parse().ok())
    })
}

fn lock_file_is_older_than(lock_path: &Path, age: Duration) -> Result<bool, Report> {
    let metadata = match fs::metadata(lock_path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(err) => {
            return Err(eyre!(
                "failed to inspect managed zcashd installation lock {}: {err}",
                lock_path.display()
            ))
        }
    };

    let Ok(modified_age) = metadata.modified()?.elapsed() else {
        return Ok(false);
    };

    Ok(modified_age >= age)
}

#[cfg(unix)]
fn process_is_running(pid: u32) -> bool {
    use nix::{errno::Errno, sys::signal::kill, unistd::Pid};

    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };

    match kill(Pid::from_raw(pid), None) {
        Ok(()) => true,
        Err(Errno::EPERM) => true,
        Err(Errno::ESRCH) => false,
        Err(_) => false,
    }
}

#[cfg(not(unix))]
fn process_is_running(_pid: u32) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
        time::Duration,
    };

    use flate2::{write::GzEncoder, Compression};
    use tar::Builder;
    use tempfile::tempdir;

    use super::{
        acquire_lock, effective_zcashd_source, normalize_member_path, provenance_matches,
        resolve_managed_zcashd_binary_from_manifest, sha256_hex_file, zcashd_target_triple, Config,
        ZcashdBinarySource,
    };
    use crate::components::zcashd_compat::{
        ConfigZcashdBinarySource, ZcashdReleaseArtifact, ZcashdReleaseManifest,
        EMBEDDED_ZCASHD_RELEASE_MANIFEST,
    };

    #[test]
    fn explicit_zcashd_path_overrides_embedded_source() {
        let config = Config {
            zcashd_source: ConfigZcashdBinarySource::Embedded,
            zcashd_path: Some("/usr/local/bin/zcashd".into()),
            ..Default::default()
        };

        assert_eq!(
            effective_zcashd_source(&config).expect("source should resolve"),
            ZcashdBinarySource::Path("/usr/local/bin/zcashd".into())
        );
    }

    #[test]
    fn path_source_requires_explicit_path() {
        let config = Config {
            zcashd_source: ConfigZcashdBinarySource::Path,
            zcashd_path: None,
            ..Default::default()
        };

        let error = effective_zcashd_source(&config).expect_err("path source should fail");
        assert!(error.to_string().contains("zcashd_source=path"));
    }

    #[test]
    fn target_triple_is_configured_or_none() {
        if let Some(target) = zcashd_target_triple() {
            assert_eq!(
                target, "x86_64-pc-linux-gnu",
                "managed zcashd downloads are only published for x86_64"
            );
            assert!(
                EMBEDDED_ZCASHD_RELEASE_MANIFEST
                    .artifact_for_target(target)
                    .is_some(),
                "managed target triple is not configured in embedded manifest: {target}"
            );
        }
    }

    #[test]
    fn normalize_member_path_strips_dot_prefix() {
        assert_eq!(normalize_member_path("./bin/zcashd"), "bin/zcashd");
        assert_eq!(normalize_member_path("bin/zcashd"), "bin/zcashd");
    }

    #[test]
    fn provenance_matches_handles_missing_and_present_files() {
        let temp = tempdir().expect("tempdir should exist");
        let path = temp.path().join("sha.txt");

        assert!(
            !provenance_matches(&path, "abc").expect("missing file should not match"),
            "missing provenance should not match"
        );

        std::fs::write(&path, "abc\n").expect("provenance file should write");
        assert!(
            provenance_matches(&path, "abc").expect("matching provenance should succeed"),
            "written provenance should match"
        );
        assert!(
            !provenance_matches(&path, "def").expect("mismatched provenance should succeed"),
            "different expected hash should not match"
        );
    }

    #[cfg(unix)]
    #[test]
    fn acquire_lock_replaces_dead_owner_lock() {
        let temp = tempdir().expect("tempdir should exist");
        let lock_path = temp.path().join(".install.lock");
        std::fs::write(&lock_path, "pid=4294967295\n").expect("lock file should write");

        let lock = acquire_lock(&lock_path, Duration::ZERO, Duration::ZERO)
            .expect("dead owner lock should recover");
        let content = std::fs::read_to_string(&lock_path).expect("lock file should be readable");

        assert!(
            content.contains(&format!("pid={}\n", std::process::id())),
            "lock file should contain current process owner: {content}"
        );

        drop(lock);
        assert!(
            !lock_path.exists(),
            "dropping the recovered lock should remove the lock file"
        );
    }

    #[test]
    fn acquire_lock_replaces_legacy_stale_lock() {
        let temp = tempdir().expect("tempdir should exist");
        let lock_path = temp.path().join(".install.lock");
        std::fs::write(&lock_path, "").expect("legacy lock file should write");

        let lock = acquire_lock(&lock_path, Duration::ZERO, Duration::ZERO)
            .expect("legacy stale lock should recover");
        let content = std::fs::read_to_string(&lock_path).expect("lock file should be readable");

        assert!(
            content.contains(&format!("pid={}\n", std::process::id())),
            "lock file should contain current process owner: {content}"
        );

        drop(lock);
        assert!(
            !lock_path.exists(),
            "dropping the recovered lock should remove the lock file"
        );
    }

    #[cfg(unix)]
    #[test]
    fn acquire_lock_keeps_live_owner_lock() {
        let temp = tempdir().expect("tempdir should exist");
        let lock_path = temp.path().join(".install.lock");
        std::fs::write(&lock_path, format!("pid={}\n", std::process::id()))
            .expect("lock file should write");

        let error = match acquire_lock(&lock_path, Duration::ZERO, Duration::ZERO) {
            Ok(_) => panic!("live owner lock should not be replaced"),
            Err(error) => error,
        };

        assert!(
            error.to_string().contains("timed out"),
            "unexpected error: {error}"
        );
        assert!(
            lock_path.exists(),
            "live owner lock should remain for its owner"
        );
    }

    #[test]
    fn managed_download_rejects_non_https_non_localhost_urls() {
        let Some(target) = zcashd_target_triple() else {
            return;
        };
        let temp = tempdir().expect("tempdir should exist");
        let manifest = ZcashdReleaseManifest {
            schema_version: 1,
            release_tag: "test-release".to_string(),
            artifacts: vec![ZcashdReleaseArtifact {
                target_triple: target.to_string(),
                runtime_archive_url: "http://example.com/zcashd.tar.gz".to_string(),
                runtime_archive_sha256:
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
                runtime_archive_member_binary_path: "./bin/zcashd".to_string(),
            }],
        };

        let error = resolve_managed_zcashd_binary_from_manifest(&manifest, temp.path())
            .expect_err("non-https URL should be rejected");
        assert!(
            error.to_string().contains("https"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn managed_resolver_downloads_and_installs_from_http_archive() {
        let Some(target) = zcashd_target_triple() else {
            return;
        };

        let temp = tempdir().expect("tempdir should exist");
        let archive_path = temp.path().join("zcashd-compat.tar.gz");
        let binary_contents = b"#!/bin/sh\necho zcashd-compat-test\n";

        {
            let file =
                std::fs::File::create(&archive_path).expect("archive file should be created");
            let encoder = GzEncoder::new(file, Compression::default());
            let mut tar = Builder::new(encoder);

            let mut header = tar::Header::new_gnu();
            header.set_path("./bin/zcashd").expect("path should be set");
            header.set_size(binary_contents.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            tar.append(&header, &binary_contents[..])
                .expect("archive member should be appended");
            tar.finish().expect("archive should finish");
        }

        let archive_sha256 = sha256_hex_file(&archive_path).expect("archive hash should compute");
        let archive_bytes = std::fs::read(&archive_path).expect("archive bytes should be readable");

        let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
        let address = listener
            .local_addr()
            .expect("listener should have local address");
        let payload = archive_bytes.clone();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("one client should connect");
            let mut request_buf = [0u8; 1024];
            let _ = stream.read(&mut request_buf);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                payload.len()
            );
            stream
                .write_all(response.as_bytes())
                .expect("response headers should write");
            stream
                .write_all(&payload)
                .expect("response body should write");
        });

        let local_http_url = format!("http://{address}/zcashd-compat.tar.gz");
        let artifact = ZcashdReleaseArtifact {
            target_triple: target.to_string(),
            runtime_archive_url: local_http_url,
            runtime_archive_sha256: archive_sha256,
            runtime_archive_member_binary_path: "./bin/zcashd".to_string(),
        };
        let manifest = ZcashdReleaseManifest {
            schema_version: 1,
            release_tag: "test-release".to_string(),
            artifacts: vec![artifact],
        };
        let resolved = resolve_managed_zcashd_binary_from_manifest(&manifest, temp.path())
            .expect("managed resolver should install zcashd");
        let installed = std::fs::read(&resolved).expect("installed zcashd should be readable");
        assert_eq!(installed, binary_contents);

        server.join().expect("server thread should exit cleanly");
    }
}
