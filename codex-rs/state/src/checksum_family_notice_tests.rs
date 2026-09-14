use pretty_assertions::assert_eq;
use std::ffi::OsStr;
use std::path::PathBuf;

use super::DriftFingerprint;
use super::NOTICE_FILE_NAME;
use super::is_notice_disabled;
use super::marker_path;
use super::read_marker;
use super::record_family_notice;
use super::should_show;
use crate::runtime::test_support::unique_temp_dir;

fn fingerprint(db: &str, bin: &str, platform: &str) -> DriftFingerprint {
    DriftFingerprint {
        db_family: db.to_string(),
        binary_family: bin.to_string(),
        platform: platform.to_string(),
    }
}

fn temp_home() -> PathBuf {
    let home = unique_temp_dir();
    std::fs::create_dir_all(&home).expect("home");
    home
}

#[test]
fn notice_shows_once_per_fingerprint_and_records_the_marker() {
    let home = temp_home();
    let _cleanup = scopeguard::guard(home.clone(), |home| {
        let _ = std::fs::remove_dir_all(home);
    });
    let fp = fingerprint("lf", "crlf", "windows");
    assert!(record_family_notice(&home, &fp), "first detection shows");
    let marker = marker_path(&home.as_path());
    assert!(marker.exists(), "marker persisted under the sqlite home");
    assert!(
        !record_family_notice(&home, &fp),
        "repeat detection stays quiet"
    );

    let stored = read_marker(home.as_path());
    let seen = stored
        .notices
        .get("family-mismatch")
        .expect("family-mismatch record");
    assert_eq!(seen.db_family, "lf");
    assert_eq!(seen.binary_family, "crlf");
    assert_eq!(seen.platform, "windows");
}

#[test]
fn fingerprint_change_rearms_the_notice() {
    let home = temp_home();
    let _cleanup = scopeguard::guard(home.clone(), |home| {
        let _ = std::fs::remove_dir_all(home);
    });
    assert!(record_family_notice(
        &home,
        &fingerprint("lf", "crlf", "windows")
    ));
    // Same drift: quiet.
    assert!(!record_family_notice(
        &home,
        &fingerprint("lf", "crlf", "windows")
    ));
    // Database rebuilt into the other family: re-arm.
    assert!(record_family_notice(
        &home,
        &fingerprint("crlf", "crlf", "windows")
    ));
    // Binary family moved (platform cross-compile / different build): re-arm.
    assert!(record_family_notice(
        &home,
        &fingerprint("crlf", "lf", "windows")
    ));
    // Same drift on another platform: re-arm.
    assert!(record_family_notice(
        &home,
        &fingerprint("crlf", "lf", "linux")
    ));
}

#[test]
fn should_show_decision_matrix() {
    let fp = fingerprint("lf", "crlf", "windows");
    assert!(should_show(None, &fp));
    let exact = seen_from(&fp, 1);
    assert!(!should_show(Some(&exact), &fp));
    let other_db = seen_from(&fingerprint("crlf", "crlf", "windows"), 1);
    assert!(should_show(Some(&other_db), &fp));
    let other_bin = seen_from(&fingerprint("lf", "lf", "windows"), 1);
    assert!(should_show(Some(&other_bin), &fp));
    let other_platform = seen_from(&fingerprint("lf", "crlf", "linux"), 1);
    assert!(should_show(Some(&other_platform), &fp));
}

fn seen_from(fp: &DriftFingerprint, at: u64) -> super::SeenNotice {
    super::SeenNotice {
        seen_at_epoch_secs: at,
        db_family: fp.db_family.clone(),
        binary_family: fp.binary_family.clone(),
        platform: fp.platform.clone(),
    }
}

#[test]
fn env_gate_matrix() {
    assert!(!is_notice_disabled(None));
    assert!(!is_notice_disabled(Some(OsStr::new(""))));
    assert!(!is_notice_disabled(Some(OsStr::new("0"))));
    assert!(is_notice_disabled(Some(OsStr::new("1"))));
    assert!(is_notice_disabled(Some(OsStr::new("false"))));
}
