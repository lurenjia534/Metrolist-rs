const MANIFEST: &str = include_str!("../Cargo.toml");
const INFO_PLIST_EXTENSION: &str = include_str!("../packaging/macos/Info.plist.ext");

#[test]
fn macos_bundle_declares_the_scoped_microphone_purpose() {
    assert!(MANIFEST.contains("osx_info_plist_exts = [\"packaging/macos/Info.plist.ext\"]"));
    assert_eq!(
        INFO_PLIST_EXTENSION
            .matches("<key>NSMicrophoneUsageDescription</key>")
            .count(),
        1
    );
    assert!(INFO_PLIST_EXTENSION.contains("only after you start song recognition"));
    assert!(INFO_PLIST_EXTENSION.contains("at most 12 seconds"));
    assert!(INFO_PLIST_EXTENSION.contains("in memory"));
    assert!(INFO_PLIST_EXTENSION.contains("encoded signature to Shazam"));
}
