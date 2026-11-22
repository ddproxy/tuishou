//! Minimal file-backed GitHub-release emulator used by `make release-check`.
//! It serves a local archive and checksum as a fake release over loopback HTTP.
//! Nothing here is part of the shipped binary.

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::thread;

fn main() {
    let mut args = std::env::args().skip(1);
    let assets_dir = PathBuf::from(args.next().expect("usage: file_release_server <dir> <tag>"));
    let tag = args.next().expect("usage: file_release_server <dir> <tag>");

    let mut names: Vec<String> = fs::read_dir(&assets_dir)
        .expect("cannot read assets dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".tar.gz"))
        .collect();
    names.sort();
    assert!(
        !names.is_empty(),
        "no .tar.gz assets in {}",
        assets_dir.display()
    );
    let asset = names[0].clone();
    let checksum = format!("{asset}.sha256");

    let listener = TcpListener::bind("127.0.0.1:0").expect("cannot bind loopback port");
    let port = listener.local_addr().unwrap().port();
    println!("{port}");
    let _ = std::io::stdout().flush();

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let dir = assets_dir.clone();
                let tag = tag.clone();
                let asset = asset.clone();
                let checksum = checksum.clone();
                thread::spawn(move || handle(stream, port, &dir, &tag, &asset, &checksum));
            }
            Err(err) => eprintln!("file_release_server: accept failed: {err}"),
        }
    }
}

fn handle(mut stream: TcpStream, port: u16, dir: &Path, tag: &str, asset: &str, checksum: &str) {
    let mut buf = [0u8; 8192];
    let n = match stream.read(&mut buf) {
        Ok(n) => n,
        Err(_) => return,
    };
    let request = String::from_utf8_lossy(&buf[..n]);
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/")
        .to_string();

    let (status, body, content_type) = if path == "/repos/ddproxy/tuishou/releases/latest"
        || path == format!("/repos/ddproxy/tuishou/releases/tags/{tag}")
    {
        let release = serde_json::json!({
            "tag_name": tag,
            "body": format!("## {tag}\n\n- release rehearsal\n"),
            "assets": [
                {"name": asset, "browser_download_url": format!("http://127.0.0.1:{port}/dl/{asset}")},
                {"name": checksum, "browser_download_url": format!("http://127.0.0.1:{port}/dl/{checksum}")}
            ]
        });
        (
            "200 OK",
            release.to_string().into_bytes(),
            "application/json",
        )
    } else if let Some(name) = path.strip_prefix("/dl/") {
        if name != asset && name != checksum {
            ("404 Not Found", b"not found".to_vec(), "text/plain")
        } else {
            match fs::read(dir.join(name)) {
                Ok(bytes) => ("200 OK", bytes, "application/octet-stream"),
                Err(_) => ("404 Not Found", b"not found".to_vec(), "text/plain"),
            }
        }
    } else {
        ("404 Not Found", b"not found".to_vec(), "text/plain")
    };

    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: {content_type}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(&body);
}
