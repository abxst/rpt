use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::PathBuf,
    process::Command,
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

fn scratch() -> PathBuf {
    let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let path = std::env::temp_dir().join(format!("rpt-diag-{}-{suffix}", std::process::id()));
    fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn missing_manifest_is_system_error_on_stderr() {
    let root = scratch();
    let output = Command::new(env!("CARGO_BIN_EXE_rpt"))
        .args(["fetch", "--manifest"])
        .arg(root.join("not-found.json"))
        .arg("--output")
        .arg(root.join("packages"))
        .output()
        .unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(!output.status.success());
    assert!(stderr.contains("rpt: error [system] SYSTEM_IO: reading manifest:"), "{stderr}");
    assert!(!stderr.contains("system: reading manifest"), "{stderr}");
    assert!(output.stdout.is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn http_404_is_download_error_on_stderr() {
    let root = scratch();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut conn, _) = listener.accept().unwrap();
        let mut buf = [0u8; 2048];
        let _ = conn.read(&mut buf).unwrap();
        conn.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
    });
    let manifest = serde_json::json!({
        "packages": [{
            "name": "missing-package",
            "url": format!("http://{address}/missing.deb"),
            "sha256": "a".repeat(64),
            "size": 1024,
            "filename": "missing.deb"
        }]
    });
    let manifest_path = root.join("manifest.json");
    fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_rpt"))
        .arg("fetch")
        .arg("--manifest").arg(&manifest_path)
        .arg("--output").arg(root.join("packages"))
        .args(["--workers", "1", "--segments", "1"])
        .output().unwrap();
    server.join().unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(!output.status.success());
    assert!(stderr.contains("[download] DOWNLOAD_HTTP package missing-package:"), "{stderr}");
    assert!(stderr.contains("404"), "{stderr}");
    assert!(stderr.contains("[download] DOWNLOAD_INTEGRITY: 1 of 1 package(s) failed"), "{stderr}");
    assert!(!stderr.contains("[system]"), "{stderr}");
    assert!(stdout.contains("1 failed"), "{stdout}");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn json_error_is_single_line_on_stderr() {
    let root = scratch();
    let output = Command::new(env!("CARGO_BIN_EXE_rpt"))
        .args(["--log-format", "json", "fetch", "--manifest"])
        .arg(root.join("missing.json"))
        .arg("--output")
        .arg(root.join("cache"))
        .output().unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    let lines: Vec<_> = stderr.lines().collect();
    assert_eq!(lines.len(), 1, "{stderr}");
    let entry: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(entry["event"], "error");
    assert_eq!(entry["category"], "system");
    assert_eq!(entry["code"], "SYSTEM_IO");
    assert_eq!(entry["package"], serde_json::Value::Null);
    assert!(entry["message"].as_str().unwrap().contains("reading manifest"));
    fs::remove_dir_all(root).unwrap();
}
