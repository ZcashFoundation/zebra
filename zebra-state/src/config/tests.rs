//! Tests for state configuration.

use super::*;

#[test]
fn redacted_string_hides_contents_from_debug() {
    let secret = RedactedString::from("hunter2");

    assert_eq!(format!("{secret:?}"), "[REDACTED]");
    assert_eq!(secret.as_str(), "hunter2");
}

#[cfg(feature = "elasticsearch")]
#[test]
fn config_debug_does_not_contain_elasticsearch_password() {
    let config = Config {
        elasticsearch_password: RedactedString::from("hunter2"),
        ..Config::default()
    };

    assert!(!format!("{config:?}").contains("hunter2"));
}

#[cfg(feature = "elasticsearch")]
#[test]
fn elasticsearch_validates_certificates_unless_skipped() {
    use elasticsearch::cert::CertificateValidation;

    let config = Config::default();
    assert!(matches!(
        config.elasticsearch_cert_validation(),
        CertificateValidation::Default
    ));

    let config = Config {
        elasticsearch_skip_cert_validation: true,
        ..Config::default()
    };
    assert!(matches!(
        config.elasticsearch_cert_validation(),
        CertificateValidation::None
    ));
}
