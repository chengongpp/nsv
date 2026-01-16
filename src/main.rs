use percent_encoding::percent_decode_str;
use ctrlc;
use std::borrow::Cow;
use std::env;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::thread;
use tiny_http::{Header, Method, Response, Server, StatusCode};
use chrono::Local;

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut force = false;
    let mut port: Option<u16> = None;

    let mut args = env::args().skip(1).peekable();
    while let Some(arg) = args.next() {
        if arg == "--force" {
            force = true;
            continue;
        }
        if arg.starts_with('-') {
            return Err(format!("Unknown flag: {arg}").into());
        }

        if port.is_none() {
            let value: u16 = arg
                .parse()
                .map_err(|_| "Port must be a number between 1 and 65535")?;
            if value == 0 {
                return Err("Port must be between 1 and 65535".into());
            }
            port = Some(value);
            continue;
        }
        return Err("Too many positional arguments".into());
    }

    let base_dir = env::current_dir()?;
    let base_dir = base_dir.canonicalize()?;

    if !force && is_dangerous_dir(&base_dir) {
        eprintln!(
            "Refusing to serve dangerous directory: {}",
            base_dir.display()
        );
        eprintln!("Pass --force to override, or pick a safer directory.");
        std::process::exit(1);
    }

    let port = port.unwrap_or(8000);
    let addr = format!("[::]:{port}");
    let server = Server::http(&addr)?;

    ctrlc::set_handler(|| {
        std::process::exit(0);
    })?;

    println!("Serving {} on http://{}", base_dir.display(), addr);
    println!("Index is disabled; only direct file paths are allowed.");

    for request in server.incoming_requests() {
        let base_dir = base_dir.clone();
        thread::spawn(move || handle_request(base_dir, request));
    }

    Ok(())
}

fn handle_request(base_dir: PathBuf, request: tiny_http::Request) {
    let method = request.method().clone();
    if method != Method::Get && method != Method::Head {
        let _ = request.respond(Response::empty(StatusCode(405)));
        return;
    }

    let remote = request
        .remote_addr()
        .map(|addr| addr.to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let url = request.url();
    let path = url.split('?').next().unwrap_or(url);
    if path == "/" || path.ends_with('/') {
        let _ = request.respond(Response::empty(StatusCode(403)));
        return;
    }

    let rel = path.trim_start_matches('/');
    if rel.is_empty() {
        let _ = request.respond(Response::empty(StatusCode(403)));
        return;
    }

    let rel = decode_path(rel);
    if rel.contains('\u{0000}') {
        let _ = request.respond(Response::empty(StatusCode(400)));
        return;
    }

    let candidate = base_dir.join(rel.as_ref());
    let candidate = match candidate.canonicalize() {
        Ok(path) => path,
        Err(_) => {
            let _ = request.respond(Response::empty(StatusCode(404)));
            return;
        }
    };

    if !candidate.starts_with(&base_dir) {
        let _ = request.respond(Response::empty(StatusCode(403)));
        return;
    }
    if candidate.is_dir() {
        let _ = request.respond(Response::empty(StatusCode(403)));
        return;
    }

    let file_name = candidate
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("download");
    let disposition = format!("attachment; filename=\"{}\"", file_name);
    let header = Header::from_bytes(&b"Content-Disposition"[..], disposition)
        .unwrap_or_else(|_| Header::from_bytes(&b"Content-Disposition"[..], "attachment")
            .expect("valid header"));

    if method == Method::Head {
        let len = match std::fs::metadata(&candidate) {
            Ok(meta) => meta.len().to_string(),
            Err(_) => {
                let _ = request.respond(Response::empty(StatusCode(404)));
                return;
            }
        };
        log_download(&remote, &candidate);
        let len_header = Header::from_bytes(&b"Content-Length"[..], len)
            .unwrap_or_else(|_| Header::from_bytes(&b"Content-Length"[..], "0")
                .expect("valid header"));
        let response = Response::empty(StatusCode(200))
            .with_header(header)
            .with_header(len_header);
        let _ = request.respond(response);
        return;
    }

    let file = match File::open(&candidate) {
        Ok(file) => file,
        Err(_) => {
            let _ = request.respond(Response::empty(StatusCode(404)));
            return;
        }
    };
    log_download(&remote, &candidate);
    let response = Response::from_file(file).with_header(header);
    let _ = request.respond(response);
}

fn decode_path(input: &str) -> Cow<'_, str> {
    if input.contains('%') {
        percent_decode_str(input).decode_utf8_lossy()
    } else {
        Cow::Borrowed(input)
    }
}

fn is_dangerous_dir(path: &Path) -> bool {
    if path.parent().is_none() {
        return true;
    }

    if let Some(home) = home_dir() {
        if path == home {
            return true;
        }
    }

    false
}

fn home_dir() -> Option<PathBuf> {
    if let Ok(home) = env::var("HOME") {
        return Some(PathBuf::from(home));
    }
    if let Ok(home) = env::var("USERPROFILE") {
        return Some(PathBuf::from(home));
    }
    None
}

fn log_download(remote: &str, file: &Path) {
    let ts = Local::now().format("%Y%m%d-%H:%M:%S%z");
    println!("ts={} ip={} file={}", ts, remote, file.display());
}
