use std::{io::{Read, Write}, net::TcpListener, process::Command, thread, time::{SystemTime, UNIX_EPOCH}};

fn temp_path(label: &str) -> std::path::PathBuf {
    let tick = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    std::env::temp_dir().join(format!("rpt-cli-test-{label}-{}-{tick}", std::process::id()))
}

#[test]
fn missing_manifest_reports_system_on_stderr() {
    let missing = temp_path("missing");
    let output = Command::new(env!("CARGO_BIN_EXE_rpt"))
        .args(["fetch", "--manifest"])
        .arg(&missing)
        .output().unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("error [system]"), "{stderr}");
    assert!(stderr.contains("reading manifest"), "{stderr}");
    assert!(output.stdout.is_empty());
}

#[test]
fn http_404_reports_download_on_stderr_and_nonzero_exit() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let mut request = [0u8; 4096];
        let _ = socket.read(&mut request);
        socket.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
    });
    let root = temp_path("download");
    std::fs::create_dir_all(&root).unwrap();
    let manifest = root.join("manifest.json");
    let body = format!(
        r#"{{"packages":[{{"name":"missing","url":"http://{address}/missing.deb","sha256":"{}","size":1,"filename":"missing.deb"}}]}}"#,
        "a".repeat(64)
    );
    std::fs::write(&manifest, body).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_rpt"))
        .args(["fetch", "--manifest"])
        .arg(&manifest)
        .args(["--output"])
        .arg(root.join("output"))
        .args(["--workers", "1", "--segments", "1"])
        .output().unwrap();
    server.join().unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stderr.contains("error [download]"), "{stderr}");
    assert!(stderr.contains("404"), "{stderr}");
    assert!(stdout.contains("1 failed"), "{stdout}");
    assert!(!stdout.contains("Downloads verified in"), "{stdout}");
    std::fs::remove_dir_all(root).unwrap();
}
