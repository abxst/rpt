use rpt::{sources::list_sources, validate_package, Package};

#[test]
fn rejects_unsafe_manifest_filename() {
    let p = Package {
        name: "test".into(),
        url: "https://example.org/test.deb".into(),
        sha256: "0".repeat(64),
        size: 10,
        filename: "/etc/passwd".into(),
    };
    assert!(validate_package(&p).is_err());
}

#[test]
fn rejects_non_http_transport() {
    let p = Package {
        name: "test".into(),
        url: "file:///etc/passwd".into(),
        sha256: "0".repeat(64),
        size: 10,
        filename: "test.deb".into(),
    };
    assert!(validate_package(&p).is_err());
}

#[test]
fn sources_are_read_only() {
    let root = std::env::temp_dir().join(format!("rpt-contract-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let list = root.join("sources.list");
    let contents = "deb https://example.org/debian stable main\n";
    std::fs::write(&list, contents).unwrap();
    let sources = list_sources(&root).unwrap();
    assert_eq!(sources, vec![contents.trim().to_owned()]);
    assert_eq!(std::fs::read_to_string(&list).unwrap(), contents);
    std::fs::remove_dir_all(root).unwrap();
}
