//! `ring deployment exec` — run a command, or open a shell, inside a
//! deployment's instance.
//!
//! The client half of the exec WebSocket. Its job is to make a remote process
//! feel local: bytes typed here reach the container unmodified, bytes it
//! writes land on this terminal unmodified, and the command's exit code
//! becomes this process's exit code.
//!
//! **Raw mode.** With a TTY the terminal is put in raw mode for the duration,
//! so Ctrl-C, arrow keys and tab completion reach the remote program instead
//! of being interpreted by the local shell. It is restored on every exit path
//! — including a broken connection — because a terminal left in raw mode
//! makes the user's session unusable afterwards.
//!
//! **Shell choice is the caller's.** The runtime deliberately does not guess
//! a shell, so the default lives here: `/bin/sh`, the one interpreter a POSIX
//! image is expected to have. `--` lets the caller run anything else.

use clap::{Arg, ArgAction, ArgMatches, Command};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use crate::config::auth::load_auth_config;
use crate::config::config::Config;
use crate::exit_code::ExitCode;

/// Client→server frame. Mirrors the server's `ClientFrame`.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientFrame {
    Stdin { data: String },
    Resize { cols: u16, rows: u16 },
}

/// Server→client frame. Mirrors the server's `ServerFrame`.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerFrame {
    Stdout { data: String },
    Stderr { data: String },
    Exit { code: Option<i64> },
    Error { message: String },
}

pub(crate) fn command_config() -> Command {
    Command::new("exec")
        .about("Run a command inside a deployment's instance")
        .arg(Arg::new("id").required(true).help("Deployment ID"))
        .arg(
            Arg::new("container")
                .long("container")
                .short('c')
                .help("Instance to enter (defaults to the first running one)"),
        )
        .arg(
            Arg::new("no-tty")
                .long("no-tty")
                .short('T')
                .help("Do not allocate a TTY (keeps stdout and stderr separate)")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("command")
                .num_args(0..)
                .last(true)
                .help("Command to run (defaults to /bin/sh)"),
        )
}

pub(crate) async fn execute(args: &ArgMatches, mut configuration: Config) {
    let id = args.get_one::<String>("id").unwrap();
    let container = args.get_one::<String>("container");

    let command: Vec<String> = args
        .get_many::<String>("command")
        .map(|values| values.cloned().collect())
        .unwrap_or_default();
    let command = if command.is_empty() {
        // The runtime refuses to guess; the product decision is made here.
        vec!["/bin/sh".to_string()]
    } else {
        command
    };

    // A TTY only makes sense when this process actually has one. Piping into
    // `ring exec` with a PTY on the far end would deliver terminal control
    // sequences into whatever consumes the output.
    let want_tty = !args.get_flag("no-tty") && stdin_is_tty() && stdout_is_tty();

    let api_url = configuration.get_api_url();
    let auth_config = load_auth_config(configuration.name.clone());

    let url = build_url(
        &api_url,
        id,
        &command,
        want_tty,
        container.map(|s| s.as_str()),
        if want_tty { terminal_size() } else { None },
    );

    let exit = run(&url, &auth_config.token, want_tty).await;

    std::process::exit(exit);
}

/// Build the exec WebSocket URL.
///
/// `command` is repeated rather than joined so an argument containing a space
/// survives: `-c "echo hi"` is three arguments, not four words.
fn build_url(
    api_url: &str,
    id: &str,
    command: &[String],
    tty: bool,
    container: Option<&str>,
    size: Option<(u16, u16)>,
) -> String {
    let scheme = if api_url.starts_with("https://") {
        "wss"
    } else {
        "ws"
    };
    let host = api_url
        .trim_start_matches("https://")
        .trim_start_matches("http://");

    let mut params: Vec<String> = command
        .iter()
        .map(|c| format!("command={}", encode_query(c)))
        .collect();

    params.push(format!("tty={}", tty));

    if let Some(c) = container {
        params.push(format!("container={}", encode_query(c)));
    }
    if let Some((cols, rows)) = size {
        params.push(format!("cols={cols}"));
        params.push(format!("rows={rows}"));
    }

    format!(
        "{}://{}/deployments/{}/exec?{}",
        scheme,
        host,
        id,
        params.join("&")
    )
}

/// Connect, relay, and return the process exit code to hand to the shell.
async fn run(url: &str, token: &str, want_tty: bool) -> i32 {
    let mut request = match url.into_client_request() {
        Ok(request) => request,
        Err(e) => {
            eprintln!("Invalid exec URL: {e}");
            return ExitCode::General as i32;
        }
    };

    // A WebSocket handshake is a normal HTTP request, so the Bearer token
    // goes in a header — no need for the `?ticket=` dance a browser requires.
    match format!("Bearer {token}").parse() {
        Ok(value) => {
            request.headers_mut().insert("Authorization", value);
        }
        Err(_) => {
            eprintln!("Invalid authentication token");
            return ExitCode::Auth as i32;
        }
    }

    let (stream, response) = match tokio_tungstenite::connect_async(request).await {
        Ok(pair) => pair,
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            let status = response.status().as_u16();
            let detail = response
                .body()
                .as_ref()
                .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok())
                .and_then(|v| v["error"].as_str().map(str::to_string));

            match detail {
                Some(message) => eprintln!("Cannot open exec session: {message}"),
                None => eprintln!("Cannot open exec session: HTTP {status}"),
            }
            return crate::exit_code::from_http_status(status) as i32;
        }
        Err(e) => {
            eprintln!("Cannot reach the Ring API: {e}");
            return ExitCode::Connection as i32;
        }
    };

    debug_assert_eq!(response.status().as_u16(), 101);

    // Raw mode goes on only once the session is actually open: bailing out
    // before this point must leave the terminal untouched.
    let restore = if want_tty { RawMode::enable() } else { None };

    let code = relay(stream).await;

    // Explicit rather than relying on drop order, so the terminal is usable
    // again before anything is printed after this point.
    drop(restore);

    code
}

async fn relay(
    stream: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> i32 {
    use tokio::io::AsyncWriteExt;

    let (mut sink, mut source) = stream.split();
    let (resize_tx, mut resize_rx) = tokio::sync::mpsc::channel::<(u16, u16)>(8);
    spawn_resize_watcher(resize_tx);

    // stdin is read on a blocking thread: reading a terminal is a blocking
    // syscall, and doing it on the async runtime would stall every other task
    // between keystrokes.
    let (stdin_tx, mut stdin_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(16);
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let mut buffer = [0u8; 4096];
        loop {
            use std::io::Read;
            match stdin.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if stdin_tx.blocking_send(buffer[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let mut stdout = tokio::io::stdout();
    let mut stderr = tokio::io::stderr();
    let mut exit_code = 0;
    // Set once local stdin is exhausted. Without it the `recv()` branch below
    // would resolve to `None` instantly and forever, and — `select!` polling a
    // ready branch every iteration — starve the output branch: the remote
    // command's stdout was already on the wire but never got printed.
    let mut stdin_done = false;

    loop {
        tokio::select! {
            // Output first: a command that writes and exits must have its
            // bytes delivered before the session is torn down.
            biased;

            incoming = source.next() => {
                let Some(Ok(message)) = incoming else { break };
                let Message::Text(text) = message else { continue };

                match serde_json::from_str::<ServerFrame>(&text) {
                    Ok(ServerFrame::Stdout { data }) => {
                        if let Some(bytes) = decode(&data) {
                            let _ = stdout.write_all(&bytes).await;
                            let _ = stdout.flush().await;
                        }
                    }
                    Ok(ServerFrame::Stderr { data }) => {
                        if let Some(bytes) = decode(&data) {
                            let _ = stderr.write_all(&bytes).await;
                            let _ = stderr.flush().await;
                        }
                    }
                    Ok(ServerFrame::Exit { code }) => {
                        // Mirror the shell convention: the remote command's
                        // status becomes ours, so `ring exec … && next` works.
                        exit_code = code.unwrap_or(0) as i32;
                        break;
                    }
                    Ok(ServerFrame::Error { message }) => {
                        let _ = stderr.write_all(format!("\r\nring: {message}\r\n").as_bytes()).await;
                        let _ = stderr.flush().await;
                        exit_code = ExitCode::General as i32;
                        break;
                    }
                    Err(_) => {}
                }
            }

            chunk = stdin_rx.recv(), if !stdin_done => {
                let Some(bytes) = chunk else {
                    // Local stdin is exhausted. Stop polling this branch, but
                    // do NOT close the socket: a WebSocket close is a
                    // *session* teardown, not an "stdin is done" signal, and
                    // sending one here raced the command's own output away.
                    // `ring exec … -- echo hi` inside `$(…)` reads EOF from
                    // /dev/null instantly, so the close went out before the
                    // container had written a byte and the result came back
                    // empty. The session ends when the server reports `exit`.
                    stdin_done = true;
                    continue;
                };
                let frame = ClientFrame::Stdin { data: encode(&bytes) };
                if send(&mut sink, &frame).await.is_err() {
                    break;
                }
            }

            size = resize_rx.recv() => {
                let Some((cols, rows)) = size else { continue };
                let _ = send(&mut sink, &ClientFrame::Resize { cols, rows }).await;
            }
        }
    }

    let _ = sink.send(Message::Close(None)).await;
    exit_code
}

async fn send<S>(sink: &mut S, frame: &ClientFrame) -> Result<(), ()>
where
    S: SinkExt<Message> + Unpin,
{
    let payload = serde_json::to_string(frame).map_err(|_| ())?;
    sink.send(Message::Text(payload.into()))
        .await
        .map_err(|_| ())
}

/// Watch for terminal resizes and forward them.
///
/// Without this a full-screen program keeps drawing at the size the window
/// had when the session opened, which is only correct until the user drags a
/// window edge.
fn spawn_resize_watcher(tx: tokio::sync::mpsc::Sender<(u16, u16)>) {
    tokio::spawn(async move {
        use tokio::signal::unix::{SignalKind, signal};

        let Ok(mut winch) = signal(SignalKind::window_change()) else {
            return;
        };

        while winch.recv().await.is_some() {
            if let Some((cols, rows)) = terminal_size()
                && tx.send((cols, rows)).await.is_err()
            {
                return;
            }
        }
    });
}

/// Terminal size in character cells, or `None` when there is no terminal.
fn terminal_size() -> Option<(u16, u16)> {
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    // SAFETY: `size` is a valid, correctly-sized winsize for TIOCGWINSZ.
    let result = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut size) };

    if result != 0 || size.ws_col == 0 || size.ws_row == 0 {
        return None;
    }
    Some((size.ws_col, size.ws_row))
}

fn stdin_is_tty() -> bool {
    // SAFETY: isatty only inspects the descriptor; no memory is touched.
    unsafe { libc::isatty(libc::STDIN_FILENO) == 1 }
}

fn stdout_is_tty() -> bool {
    // SAFETY: as above.
    unsafe { libc::isatty(libc::STDOUT_FILENO) == 1 }
}

/// Raw mode, restored on drop.
///
/// Drop is what makes this safe to use: a panic, a `?`, or a dropped
/// connection all unwind through it, and the terminal comes back. Restoring
/// only on the happy path would leave a user with an unusable shell whenever
/// the session ended badly.
struct RawMode {
    original: libc::termios,
}

impl RawMode {
    fn enable() -> Option<Self> {
        let mut original: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: `original` is a valid termios for this descriptor.
        if unsafe { libc::tcgetattr(libc::STDIN_FILENO, &mut original) } != 0 {
            return None;
        }

        let mut raw = original;
        // SAFETY: cfmakeraw only rewrites the struct we own.
        unsafe { libc::cfmakeraw(&mut raw) };
        if unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw) } != 0 {
            return None;
        }

        Some(RawMode { original })
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        // SAFETY: restoring the exact struct read in `enable`.
        unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.original) };
    }
}

fn encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn decode(data: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(data).ok()
}

fn encode_query(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{:02X}", byte)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn url_repeats_command_so_argv_survives() {
        let url = build_url(
            "http://localhost:3030",
            "abc",
            &cmd(&["/bin/sh", "-c", "echo hi"]),
            true,
            None,
            None,
        );

        // The space must be encoded inside one parameter, not split into two:
        // `sh -c "echo hi"` is three arguments.
        assert!(url.contains("command=%2Fbin%2Fsh"));
        assert!(url.contains("command=-c"));
        assert!(url.contains("command=echo%20hi"));
    }

    #[test]
    fn https_api_upgrades_to_wss() {
        let url = build_url(
            "https://ring.example.com",
            "abc",
            &cmd(&["/bin/sh"]),
            true,
            None,
            None,
        );
        assert!(url.starts_with("wss://ring.example.com/deployments/abc/exec?"));
    }

    #[test]
    fn http_api_uses_plain_ws() {
        let url = build_url(
            "http://localhost:3030",
            "abc",
            &cmd(&["/bin/sh"]),
            true,
            None,
            None,
        );
        assert!(url.starts_with("ws://localhost:3030/deployments/abc/exec?"));
    }

    #[test]
    fn size_is_sent_only_when_known() {
        let with = build_url(
            "http://h",
            "abc",
            &cmd(&["/bin/sh"]),
            true,
            None,
            Some((120, 40)),
        );
        assert!(with.contains("cols=120"));
        assert!(with.contains("rows=40"));

        let without = build_url("http://h", "abc", &cmd(&["/bin/sh"]), true, None, None);
        assert!(!without.contains("cols="));
        assert!(!without.contains("rows="));
    }

    #[test]
    fn container_is_encoded() {
        let url = build_url(
            "http://h",
            "abc",
            &cmd(&["/bin/sh"]),
            true,
            Some("web_app_1"),
            None,
        );
        assert!(url.contains("container=web_app_1"));
    }

    #[test]
    fn tty_flag_is_explicit_in_the_url() {
        let on = build_url("http://h", "abc", &cmd(&["/bin/sh"]), true, None, None);
        assert!(on.contains("tty=true"));

        let off = build_url("http://h", "abc", &cmd(&["/bin/sh"]), false, None, None);
        assert!(off.contains("tty=false"));
    }
}
