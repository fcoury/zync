use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

type Result<T> = std::result::Result<T, String>;

const MAGIC: &str = "ZYNC1";
const DEFAULT_INTERVAL_MS: u64 = 750;
const DEFAULT_REMOTE_BIN: &str = "zync";
const DEFAULT_SSH_BIN: &str = "ssh";

#[derive(Clone, Debug, PartialEq, Eq)]
enum ClipboardKind {
    Text,
    Image,
}

impl ClipboardKind {
    fn as_str(&self) -> &'static str {
        match self {
            ClipboardKind::Text => "text",
            ClipboardKind::Image => "image",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "text" => Ok(ClipboardKind::Text),
            "image" => Ok(ClipboardKind::Image),
            _ => Err(format!("unsupported clipboard kind: {value}")),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ClipboardItem {
    kind: ClipboardKind,
    mime: String,
    bytes: Vec<u8>,
    hash: String,
}

#[derive(Clone, Debug)]
struct ServeConfig {
    peer: String,
    remote_bin: String,
    ssh_bin: String,
    interval: Duration,
}

fn main() {
    if let Err(error) = run(env::args().collect()) {
        eprintln!("zync: {error}");
        std::process::exit(1);
    }
}

fn run(args: Vec<String>) -> Result<()> {
    match args.get(1).map(String::as_str) {
        Some("connect") => connect(parse_connect_config(&args[2..])?),
        Some("agent") => agent(),
        Some("serve") => serve(parse_serve_config(&args[2..])?),
        Some("send-once") => send_once(parse_serve_config(&args[2..])?),
        Some("receive") => receive(),
        Some("-h" | "--help") | None => {
            print_usage();
            Ok(())
        }
        Some(command) => Err(format!("unknown command: {command}")),
    }
}

fn parse_connect_config(args: &[String]) -> Result<ServeConfig> {
    let mut args = args.to_vec();
    if args.is_empty() {
        return Err(
            "connect requires a peer, for example: zync connect user@other-mac".to_string(),
        );
    }
    if !args.first().is_some_and(|arg| arg.starts_with('-')) {
        let peer = args.remove(0);
        args.splice(0..0, ["--peer".to_string(), peer]);
    }
    parse_serve_config(&args)
}

fn parse_serve_config(args: &[String]) -> Result<ServeConfig> {
    let mut peer = None;
    let mut remote_bin = DEFAULT_REMOTE_BIN.to_string();
    let mut ssh_bin = DEFAULT_SSH_BIN.to_string();
    let mut interval_ms = DEFAULT_INTERVAL_MS;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--peer" => {
                index += 1;
                peer = Some(expect_value(args, index, "--peer")?.to_string());
            }
            "--remote-bin" => {
                index += 1;
                remote_bin = expect_value(args, index, "--remote-bin")?.to_string();
            }
            "--ssh-bin" => {
                index += 1;
                ssh_bin = expect_value(args, index, "--ssh-bin")?.to_string();
            }
            "--interval-ms" => {
                index += 1;
                interval_ms = expect_value(args, index, "--interval-ms")?
                    .parse()
                    .map_err(|_| "--interval-ms must be a positive integer".to_string())?;
            }
            flag => return Err(format!("unknown option: {flag}")),
        }
        index += 1;
    }

    let peer = peer.ok_or_else(|| "--peer is required".to_string())?;
    Ok(ServeConfig {
        peer,
        remote_bin,
        ssh_bin,
        interval: Duration::from_millis(interval_ms),
    })
}

fn expect_value<'a>(args: &'a [String], index: usize, flag: &str) -> Result<&'a str> {
    args.get(index)
        .map(String::as_str)
        .ok_or_else(|| format!("{flag} requires a value"))
}

fn print_usage() {
    println!(
        "\
zync - clipboard sync over SSH for macOS

USAGE:
  zync connect user@other-mac [--remote-bin zync] [--ssh-bin ssh] [--interval-ms 750]
  zync serve --peer user@other-mac [--remote-bin zync] [--ssh-bin ssh] [--interval-ms 750]
  zync send-once --peer user@other-mac [--remote-bin zync] [--ssh-bin ssh]
  zync agent
  zync receive

Run `zync connect ...` on the Mac where you want to paste."
    );
}

fn connect(config: ServeConfig) -> Result<()> {
    require_macos()?;
    eprintln!("zync: connecting to {}", config.peer);
    let mut child = Command::new(&config.ssh_bin)
        .arg(&config.peer)
        .arg(&config.remote_bin)
        .arg("agent")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|error| format!("failed to start ssh: {error}"))?;

    let child_stdin = child
        .stdin
        .take()
        .ok_or_else(|| "failed to open ssh stdin".to_string())?;
    let child_stdout = child
        .stdout
        .take()
        .ok_or_else(|| "failed to open ssh stdout".to_string())?;
    let interval = config.interval;
    let stop = Arc::new(AtomicBool::new(false));
    let sender_stop = Arc::clone(&stop);
    let receiver_stop = Arc::clone(&stop);
    let sender = thread::spawn(move || {
        pump_clipboard_to_writer(child_stdin, interval, "local", sender_stop)
    });
    let receiver = thread::spawn(move || {
        let result = pump_reader_to_clipboard(child_stdout, "remote");
        receiver_stop.store(true, Ordering::SeqCst);
        result
    });

    let status = child
        .wait()
        .map_err(|error| format!("failed to wait for ssh: {error}"))?;
    stop.store(true, Ordering::SeqCst);
    let send_result = sender
        .join()
        .map_err(|_| "local clipboard sender panicked".to_string())?;
    let receive_result = receiver
        .join()
        .map_err(|_| "remote clipboard receiver panicked".to_string())?;

    send_result?;
    receive_result?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("ssh exited with {status}"))
    }
}

fn agent() -> Result<()> {
    require_macos()?;
    eprintln!("zync: agent started");
    let stop = Arc::new(AtomicBool::new(false));
    let sender_stop = Arc::clone(&stop);
    let receiver_stop = Arc::clone(&stop);
    let sender = thread::spawn(move || {
        pump_clipboard_to_writer(
            std::io::stdout(),
            Duration::from_millis(DEFAULT_INTERVAL_MS),
            "agent",
            sender_stop,
        )
    });
    let receiver = thread::spawn(move || {
        let result = pump_reader_to_clipboard(std::io::stdin(), "peer");
        receiver_stop.store(true, Ordering::SeqCst);
        result
    });

    let receive_result = receiver
        .join()
        .map_err(|_| "agent receiver panicked".to_string())?;
    let send_result = sender
        .join()
        .map_err(|_| "agent sender panicked".to_string())?;
    receive_result?;
    send_result
}

fn serve(config: ServeConfig) -> Result<()> {
    require_macos()?;
    eprintln!("zync: syncing clipboard to {}", config.peer);
    let mut last_seen_hash = None;

    loop {
        match read_clipboard() {
            Ok(Some(item)) => {
                if last_seen_hash.as_deref() == Some(item.hash.as_str()) {
                    thread::sleep(config.interval);
                    continue;
                }

                if should_suppress_send(&item.hash)? {
                    eprintln!("zync: accepted remote clipboard {}", item.hash);
                    last_seen_hash = Some(item.hash);
                    thread::sleep(config.interval);
                    continue;
                }

                let hash = item.hash.clone();
                match send_item(&config, &item) {
                    Ok(()) => eprintln!("zync: sent {} clipboard {hash}", item.kind.as_str()),
                    Err(error) => eprintln!("zync: send failed for {hash}: {error}"),
                }
                last_seen_hash = Some(hash);
            }
            Ok(None) => {}
            Err(error) => eprintln!("zync: clipboard read failed: {error}"),
        }

        thread::sleep(config.interval);
    }
}

fn pump_clipboard_to_writer<W: Write>(
    mut writer: W,
    interval: Duration,
    label: &str,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    let mut last_seen_hash = None;

    while !stop.load(Ordering::SeqCst) {
        match read_clipboard() {
            Ok(Some(item)) => {
                if last_seen_hash.as_deref() == Some(item.hash.as_str()) {
                    thread::sleep(interval);
                    continue;
                }

                if should_suppress_send(&item.hash)? {
                    eprintln!("zync: accepted {label} clipboard {}", item.hash);
                    last_seen_hash = Some(item.hash);
                    thread::sleep(interval);
                    continue;
                }

                writer
                    .write_all(&encode_frame(&item))
                    .and_then(|_| writer.flush())
                    .map_err(|error| format!("failed to write clipboard frame: {error}"))?;
                eprintln!("zync: sent {} clipboard {}", item.kind.as_str(), item.hash);
                last_seen_hash = Some(item.hash);
            }
            Ok(None) => {}
            Err(error) => eprintln!("zync: clipboard read failed: {error}"),
        }

        thread::sleep(interval);
    }

    Ok(())
}

fn pump_reader_to_clipboard<R: Read>(reader: R, label: &str) -> Result<()> {
    let mut reader = BufReader::new(reader);
    while let Some(item) = read_frame_from(&mut reader)? {
        write_clipboard(&item)?;
        write_received_marker(&item.hash)?;
        eprintln!(
            "zync: received {label} {} clipboard {}",
            item.kind.as_str(),
            item.hash
        );
    }
    Ok(())
}

fn send_once(config: ServeConfig) -> Result<()> {
    require_macos()?;
    let item = read_clipboard()?.ok_or_else(|| "clipboard is empty or unsupported".to_string())?;
    send_item(&config, &item)
}

fn send_item(config: &ServeConfig, item: &ClipboardItem) -> Result<()> {
    let frame = encode_frame(item);
    let mut child = Command::new(&config.ssh_bin)
        .arg(&config.peer)
        .arg(&config.remote_bin)
        .arg("receive")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("failed to start ssh: {error}"))?;

    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| "failed to open ssh stdin".to_string())?;
        stdin
            .write_all(&frame)
            .map_err(|error| format!("failed to write ssh payload: {error}"))?;
    }

    let output = child
        .wait_with_output()
        .map_err(|error| format!("failed to wait for ssh: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "ssh exited with {}; {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn receive() -> Result<()> {
    require_macos()?;
    let mut input = Vec::new();
    std::io::stdin()
        .read_to_end(&mut input)
        .map_err(|error| format!("failed to read stdin: {error}"))?;
    let item = decode_frame(&input)?;
    write_clipboard(&item)?;
    write_received_marker(&item.hash)?;
    eprintln!(
        "zync: received {} clipboard {}",
        item.kind.as_str(),
        item.hash
    );
    Ok(())
}

fn read_clipboard() -> Result<Option<ClipboardItem>> {
    let info = clipboard_info()?;
    if contains_any(&info, &["PNGf", "PNG picture"]) {
        return Ok(Some(read_image_clipboard("PNGf", "image/png", "png")?));
    }
    if contains_any(&info, &["JPEG", "JPEG picture"]) {
        return Ok(Some(read_image_clipboard("JPEG", "image/jpeg", "jpg")?));
    }
    if contains_any(&info, &["TIFF", "TIFF picture"]) {
        return Ok(Some(read_image_clipboard("TIFF", "image/tiff", "tiff")?));
    }
    if contains_any(&info, &["utf8", "ut16", "TEXT", "string"]) {
        return Ok(Some(read_text_clipboard()?));
    }
    Ok(None)
}

fn clipboard_info() -> Result<String> {
    let output = Command::new("osascript")
        .arg("-e")
        .arg("clipboard info")
        .output()
        .map_err(|error| format!("failed to run osascript: {error}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        Err(format!(
            "clipboard info failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

fn read_text_clipboard() -> Result<ClipboardItem> {
    let output = Command::new("pbpaste")
        .output()
        .map_err(|error| format!("failed to run pbpaste: {error}"))?;
    if !output.status.success() {
        return Err(format!("pbpaste exited with {}", output.status));
    }
    let bytes = output.stdout;
    let mime = "text/plain; charset=utf-8".to_string();
    let hash = fingerprint(ClipboardKind::Text.as_str(), &mime, &bytes);
    Ok(ClipboardItem {
        kind: ClipboardKind::Text,
        mime,
        bytes,
        hash,
    })
}

fn read_image_clipboard(class_code: &str, mime: &str, extension: &str) -> Result<ClipboardItem> {
    let path = temp_path(extension);
    let class_expr = apple_class_expr(class_code);
    let script = vec![
        format!("set rawData to the clipboard as {class_expr}"),
        format!("set outFile to POSIX file \"{}\"", path.display()),
        "set openedFile to open for access outFile with write permission".to_string(),
        "set eof openedFile to 0".to_string(),
        "write rawData to openedFile".to_string(),
        "close access openedFile".to_string(),
    ];

    run_osascript_lines(&script)?;
    let bytes = fs::read(&path).map_err(|error| {
        format!(
            "failed to read temporary clipboard image {}: {error}",
            path.display()
        )
    })?;
    let _ = fs::remove_file(&path);

    let mime = mime.to_string();
    let hash = fingerprint(ClipboardKind::Image.as_str(), &mime, &bytes);
    Ok(ClipboardItem {
        kind: ClipboardKind::Image,
        mime,
        bytes,
        hash,
    })
}

fn write_clipboard(item: &ClipboardItem) -> Result<()> {
    match item.kind {
        ClipboardKind::Text => write_text_clipboard(&item.bytes),
        ClipboardKind::Image => write_image_clipboard(item),
    }
}

fn write_text_clipboard(bytes: &[u8]) -> Result<()> {
    let mut child = Command::new("pbcopy")
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|error| format!("failed to run pbcopy: {error}"))?;
    child
        .stdin
        .as_mut()
        .ok_or_else(|| "failed to open pbcopy stdin".to_string())?
        .write_all(bytes)
        .map_err(|error| format!("failed to write pbcopy stdin: {error}"))?;
    let status = child
        .wait()
        .map_err(|error| format!("failed to wait for pbcopy: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("pbcopy exited with {status}"))
    }
}

fn write_image_clipboard(item: &ClipboardItem) -> Result<()> {
    let (class_code, extension) = match item.mime.as_str() {
        "image/png" => ("PNGf", "png"),
        "image/jpeg" => ("JPEG", "jpg"),
        "image/tiff" => ("TIFF", "tiff"),
        mime => return Err(format!("unsupported image MIME type: {mime}")),
    };
    let path = temp_path(extension);
    fs::write(&path, &item.bytes).map_err(|error| {
        format!(
            "failed to write temporary image {}: {error}",
            path.display()
        )
    })?;

    let class_expr = apple_class_expr(class_code);
    let script = vec![
        format!("set inFile to POSIX file \"{}\"", path.display()),
        format!("set imageData to read inFile as {class_expr}"),
        "set the clipboard to imageData".to_string(),
    ];
    let result = run_osascript_lines(&script);
    let _ = fs::remove_file(&path);
    result
}

fn run_osascript_lines(lines: &[String]) -> Result<()> {
    let mut command = Command::new("osascript");
    for line in lines {
        command.arg("-e").arg(line);
    }
    let output = command
        .output()
        .map_err(|error| format!("failed to run osascript: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "osascript exited with {}; {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn apple_class_expr(class_code: &str) -> String {
    format!("\u{00ab}class {class_code}\u{00bb}")
}

fn temp_path(extension: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    env::temp_dir().join(format!("zync-{}-{nanos}.{extension}", std::process::id()))
}

fn encode_frame(item: &ClipboardItem) -> Vec<u8> {
    let header = format!(
        "{MAGIC}\nkind:{}\nmime:{}\nhash:{}\nbytes:{}\n\n",
        item.kind.as_str(),
        item.mime,
        item.hash,
        item.bytes.len()
    );
    let mut frame = header.into_bytes();
    frame.extend_from_slice(&item.bytes);
    frame
}

fn decode_frame(input: &[u8]) -> Result<ClipboardItem> {
    let split = input
        .windows(2)
        .position(|window| window == b"\n\n")
        .ok_or_else(|| "invalid frame: missing header terminator".to_string())?;
    let header = std::str::from_utf8(&input[..split])
        .map_err(|error| format!("invalid frame header encoding: {error}"))?;
    let payload = &input[split + 2..];

    let mut lines = header.lines();
    if lines.next() != Some(MAGIC) {
        return Err("invalid frame: bad magic".to_string());
    }

    let mut kind = None;
    let mut mime = None;
    let mut hash = None;
    let mut bytes_len = None;
    for line in lines {
        let (key, value) = line
            .split_once(':')
            .ok_or_else(|| format!("invalid frame header line: {line}"))?;
        match key {
            "kind" => kind = Some(ClipboardKind::parse(value)?),
            "mime" => mime = Some(value.to_string()),
            "hash" => hash = Some(value.to_string()),
            "bytes" => {
                bytes_len = Some(
                    value
                        .parse::<usize>()
                        .map_err(|_| "invalid frame byte length".to_string())?,
                );
            }
            _ => return Err(format!("unknown frame header: {key}")),
        }
    }

    let kind = kind.ok_or_else(|| "invalid frame: missing kind".to_string())?;
    let mime = mime.ok_or_else(|| "invalid frame: missing mime".to_string())?;
    let hash = hash.ok_or_else(|| "invalid frame: missing hash".to_string())?;
    let bytes_len = bytes_len.ok_or_else(|| "invalid frame: missing bytes".to_string())?;
    if payload.len() != bytes_len {
        return Err(format!(
            "invalid frame: expected {bytes_len} payload bytes, got {}",
            payload.len()
        ));
    }

    let actual_hash = fingerprint(kind.as_str(), &mime, payload);
    if actual_hash != hash {
        return Err(format!(
            "invalid frame: hash mismatch, expected {hash}, got {actual_hash}"
        ));
    }

    Ok(ClipboardItem {
        kind,
        mime,
        bytes: payload.to_vec(),
        hash,
    })
}

fn read_frame_from<R: BufRead>(reader: &mut R) -> Result<Option<ClipboardItem>> {
    let mut first_line = String::new();
    if reader
        .read_line(&mut first_line)
        .map_err(|error| format!("failed to read frame header: {error}"))?
        == 0
    {
        return Ok(None);
    }

    if first_line.trim_end_matches(['\r', '\n']) != MAGIC {
        return Err("invalid frame: bad magic".to_string());
    }

    let mut kind = None;
    let mut mime = None;
    let mut hash = None;
    let mut bytes_len = None;

    loop {
        let mut line = String::new();
        if reader
            .read_line(&mut line)
            .map_err(|error| format!("failed to read frame header: {error}"))?
            == 0
        {
            return Err("invalid frame: unexpected end of header".to_string());
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        let (key, value) = line
            .split_once(':')
            .ok_or_else(|| format!("invalid frame header line: {line}"))?;
        match key {
            "kind" => kind = Some(ClipboardKind::parse(value)?),
            "mime" => mime = Some(value.to_string()),
            "hash" => hash = Some(value.to_string()),
            "bytes" => {
                bytes_len = Some(
                    value
                        .parse::<usize>()
                        .map_err(|_| "invalid frame byte length".to_string())?,
                );
            }
            _ => return Err(format!("unknown frame header: {key}")),
        }
    }

    let kind = kind.ok_or_else(|| "invalid frame: missing kind".to_string())?;
    let mime = mime.ok_or_else(|| "invalid frame: missing mime".to_string())?;
    let hash = hash.ok_or_else(|| "invalid frame: missing hash".to_string())?;
    let bytes_len = bytes_len.ok_or_else(|| "invalid frame: missing bytes".to_string())?;
    let mut payload = vec![0; bytes_len];
    reader
        .read_exact(&mut payload)
        .map_err(|error| format!("failed to read frame payload: {error}"))?;

    let actual_hash = fingerprint(kind.as_str(), &mime, &payload);
    if actual_hash != hash {
        return Err(format!(
            "invalid frame: hash mismatch, expected {hash}, got {actual_hash}"
        ));
    }

    Ok(Some(ClipboardItem {
        kind,
        mime,
        bytes: payload,
        hash,
    }))
}

fn fingerprint(kind: &str, mime: &str, bytes: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in kind
        .as_bytes()
        .iter()
        .chain([0].iter())
        .chain(mime.as_bytes())
        .chain([0].iter())
        .chain(bytes.iter())
    {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn state_dir() -> Result<PathBuf> {
    let home = env::var_os("HOME").ok_or_else(|| "HOME is not set".to_string())?;
    Ok(Path::new(&home).join(".zync"))
}

fn received_marker_path() -> Result<PathBuf> {
    Ok(state_dir()?.join("last-received"))
}

fn write_received_marker(hash: &str) -> Result<()> {
    let dir = state_dir()?;
    fs::create_dir_all(&dir).map_err(|error| {
        format!(
            "failed to create state directory {}: {error}",
            dir.display()
        )
    })?;
    let path = received_marker_path()?;
    fs::write(&path, hash).map_err(|error| {
        format!(
            "failed to write received marker {}: {error}",
            path.display()
        )
    })
}

fn should_suppress_send(hash: &str) -> Result<bool> {
    let path = received_marker_path()?;
    let marker = match fs::read_to_string(&path) {
        Ok(marker) => marker,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(format!(
                "failed to read received marker {}: {error}",
                path.display()
            ));
        }
    };

    if marker.trim() == hash {
        let _ = fs::remove_file(&path);
        Ok(true)
    } else {
        Ok(false)
    }
}

fn require_macos() -> Result<()> {
    if cfg!(target_os = "macos") {
        Ok(())
    } else {
        Err("zync currently supports macOS only".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_round_trips_text_payloads() {
        let bytes = b"hello from zync".to_vec();
        let mime = "text/plain; charset=utf-8".to_string();
        let item = ClipboardItem {
            kind: ClipboardKind::Text,
            hash: fingerprint(ClipboardKind::Text.as_str(), &mime, &bytes),
            mime,
            bytes,
        };

        assert_eq!(decode_frame(&encode_frame(&item)).unwrap(), item);
    }

    #[test]
    fn frame_round_trips_binary_payloads() {
        let bytes = vec![0, 1, 2, b'\n', b'\n', 255];
        let mime = "image/png".to_string();
        let item = ClipboardItem {
            kind: ClipboardKind::Image,
            hash: fingerprint(ClipboardKind::Image.as_str(), &mime, &bytes),
            mime,
            bytes,
        };

        assert_eq!(decode_frame(&encode_frame(&item)).unwrap(), item);
    }

    #[test]
    fn frame_rejects_hash_mismatch() {
        let input = b"ZYNC1\nkind:text\nmime:text/plain; charset=utf-8\nhash:bad\nbytes:3\n\nabc";
        assert!(decode_frame(input).unwrap_err().contains("hash mismatch"));
    }

    #[test]
    fn streaming_reader_reads_multiple_frames() {
        let text_bytes = b"one".to_vec();
        let text_mime = "text/plain; charset=utf-8".to_string();
        let text = ClipboardItem {
            kind: ClipboardKind::Text,
            hash: fingerprint(ClipboardKind::Text.as_str(), &text_mime, &text_bytes),
            mime: text_mime,
            bytes: text_bytes,
        };
        let image_bytes = vec![0, 1, 2, 3];
        let image_mime = "image/png".to_string();
        let image = ClipboardItem {
            kind: ClipboardKind::Image,
            hash: fingerprint(ClipboardKind::Image.as_str(), &image_mime, &image_bytes),
            mime: image_mime,
            bytes: image_bytes,
        };

        let mut stream = encode_frame(&text);
        stream.extend_from_slice(&encode_frame(&image));
        let mut reader = BufReader::new(stream.as_slice());

        assert_eq!(read_frame_from(&mut reader).unwrap(), Some(text));
        assert_eq!(read_frame_from(&mut reader).unwrap(), Some(image));
        assert_eq!(read_frame_from(&mut reader).unwrap(), None);
    }
}
