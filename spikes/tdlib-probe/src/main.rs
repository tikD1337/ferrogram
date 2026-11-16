//! Headless TDLib probe: signs in, synchronizes, then idles so the process can be measured.
//!
//! The process contains TDLib and almost nothing else, so its memory and CPU are an upper
//! bound on what the protocol engine costs the future client.
//!
//! Options:
//!   --run-sec N     seconds to stay running after sign-in completes (default 900)
//!   --qr           sign in by scanning a QR code with the Telegram app on a phone,
//!                  instead of receiving a login code
//!   --data-dir DIR  TDLib database directory (default: `tdlib-data` next to `.env`)
//!
//! Credentials come from `TG_API_ID` / `TG_API_HASH` in the environment or in the nearest
//! `.env` found by walking up from the current directory.
//!
//! Privacy: the phone number, login code and password are typed by the person running the
//! probe and go straight to TDLib. Input is echoed, so clear the terminal afterwards. The probe prints update *types* and counters only, never
//! message contents, names or identifiers.

mod dpapi;

use std::collections::BTreeMap;
use std::error::Error;
use std::ffi::{CStr, CString, c_char, c_int};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use base64::Engine;
use serde_json::{Value, json};

#[link(name = "tdjson")]
unsafe extern "C" {
    fn td_create_client_id() -> c_int;
    fn td_send(client_id: c_int, request: *const c_char);
    fn td_receive(timeout: f64) -> *const c_char;
    fn td_execute(request: *const c_char) -> *const c_char;
}

fn send(client: c_int, request: &Value) -> Result<(), Box<dyn Error>> {
    let text = CString::new(request.to_string())?;
    // SAFETY: `text` is a valid NUL-terminated string that outlives the call; TDLib copies it.
    unsafe { td_send(client, text.as_ptr()) };
    Ok(())
}

fn execute(request: &Value) -> Result<(), Box<dyn Error>> {
    let text = CString::new(request.to_string())?;
    // SAFETY: as in `send`. The returned pointer is ignored.
    unsafe { td_execute(text.as_ptr()) };
    Ok(())
}

/// Blocks inside TDLib until an event arrives or the timeout passes. No polling.
fn receive(timeout: Duration) -> Option<String> {
    // SAFETY: TDLib returns either null or a NUL-terminated string that stays valid until the
    // next `td_receive` call on this thread; it is copied before returning.
    unsafe {
        let pointer = td_receive(timeout.as_secs_f64());
        (!pointer.is_null()).then(|| CStr::from_ptr(pointer).to_string_lossy().into_owned())
    }
}

struct Args {
    run: Duration,
    qr: bool,
    data_dir: Option<PathBuf>,
}

fn parse_args() -> Result<Args, Box<dyn Error>> {
    let mut args = Args {
        run: Duration::from_secs(900),
        qr: false,
        data_dir: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || {
            it.next()
                .ok_or_else(|| format!("{flag}: a value is required"))
        };
        match flag.as_str() {
            "--qr" => args.qr = true,
            "--run-sec" => args.run = Duration::from_secs(value()?.parse()?),
            "--data-dir" => args.data_dir = Some(PathBuf::from(value()?)),
            other => return Err(format!("unknown option: {other}").into()),
        }
    }
    Ok(args)
}

fn find_env_file() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let candidate = dir.join(".env");
        if candidate.is_file() {
            return Some(candidate);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Returns the API id, the API hash and the directory the credentials were found in.
fn load_credentials() -> Result<(i64, String, PathBuf), Box<dyn Error>> {
    let mut id = std::env::var("TG_API_ID").ok();
    let mut hash = std::env::var("TG_API_HASH").ok();
    let mut root = std::env::current_dir()?;

    if (id.is_none() || hash.is_none())
        && let Some(path) = find_env_file()
    {
        for line in std::fs::read_to_string(&path)?.lines() {
            match line.trim().split_once('=') {
                Some(("TG_API_ID", value)) => id = Some(value.trim().to_owned()),
                Some(("TG_API_HASH", value)) => hash = Some(value.trim().to_owned()),
                _ => {}
            }
        }
        if let Some(parent) = path.parent() {
            root = parent.to_path_buf();
        }
    }

    let id = id
        .filter(|value| !value.is_empty())
        .ok_or("TG_API_ID is not set")?;
    let hash = hash
        .filter(|value| !value.is_empty())
        .ok_or("TG_API_HASH is not set")?;
    Ok((id.parse()?, hash, root))
}

/// Loads the database key, creating a random one on first run. Stored DPAPI-protected.
fn database_key(data_dir: &Path) -> Result<Vec<u8>, Box<dyn Error>> {
    let path = data_dir.join("key.dpapi");
    if path.is_file() {
        return Ok(dpapi::unprotect(&std::fs::read(&path)?)?);
    }
    let mut key = vec![0_u8; 32];
    getrandom::fill(&mut key).map_err(|err| format!("random generator failed: {err}"))?;
    std::fs::write(&path, dpapi::protect(&key)?)?;
    Ok(key)
}

/// Turns console echo back on.
///
/// An earlier version of the probe read the code with echo disabled; interrupting it at that
/// prompt left the terminal in no-echo mode, so nothing typed afterwards was visible.
fn enable_console_echo() {
    use windows_sys::Win32::System::Console::{
        ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT, GetConsoleMode, GetStdHandle,
        STD_INPUT_HANDLE, SetConsoleMode,
    };
    // SAFETY: the handle comes from the system and `mode` is a valid out-parameter. Both calls
    // fail harmlessly when standard input is not a console.
    unsafe {
        let handle = GetStdHandle(STD_INPUT_HANDLE);
        let mut mode = 0;
        if GetConsoleMode(handle, &mut mode) != 0 {
            SetConsoleMode(
                handle,
                mode | ENABLE_ECHO_INPUT | ENABLE_LINE_INPUT | ENABLE_PROCESSED_INPUT,
            );
        }
    }
}

/// Prints a QR code for the login link. It carries a short-lived token, not the session.
fn print_qr(link: &str) -> Result<(), Box<dyn Error>> {
    use qrcode::render::unicode::Dense1x2;
    let code = qrcode::QrCode::new(link.as_bytes())?;
    let picture = code
        .render::<Dense1x2>()
        .dark_color(Dense1x2::Light)
        .light_color(Dense1x2::Dark)
        .build();
    println!("{picture}");
    println!("Scan with Telegram on your phone: Settings > Devices > Link Desktop Device.");
    println!("The code refreshes by itself every few dozen seconds.");
    Ok(())
}

fn request_qr(client: c_int) -> Result<(), Box<dyn Error>> {
    send(
        client,
        &json!({"@type": "requestQrCodeAuthentication", "other_user_ids": []}),
    )
}

fn prompt_visible(label: &str) -> io::Result<String> {
    print!("{label}");
    io::stdout().flush()?;
    let mut line = String::new();
    if io::stdin().read_line(&mut line)? == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "no interactive input",
        ));
    }
    Ok(line.trim().to_owned())
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = parse_args()?;
    enable_console_echo();
    let (api_id, api_hash, root) = load_credentials()?;
    let data_dir = args.data_dir.unwrap_or_else(|| root.join("tdlib-data"));
    std::fs::create_dir_all(&data_dir)?;
    let key = database_key(&data_dir)?;

    execute(&json!({"@type": "setLogVerbosityLevel", "new_verbosity_level": 1}))?;
    // SAFETY: no preconditions.
    let client = unsafe { td_create_client_id() };
    // Any request starts the client; the answer arrives as ordinary events.
    send(client, &json!({"@type": "getOption", "name": "version"}))?;

    println!("tdlib-probe pid {}", std::process::id());
    println!("database: {}", data_dir.display());

    let started = Instant::now();
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    let mut ready_at: Option<Instant> = None;
    let mut closing = false;
    let mut bytes_received: u64 = 0;

    loop {
        let mut timeout = Duration::from_secs(60);
        if let Some(ready) = ready_at
            && !closing
        {
            let left = args.run.saturating_sub(ready.elapsed());
            if left.is_zero() {
                println!("run time elapsed, closing");
                send(client, &json!({"@type": "close"}))?;
                closing = true;
            } else {
                timeout = timeout.min(left);
            }
        }

        let Some(text) = receive(timeout) else {
            continue;
        };
        bytes_received += text.len() as u64;
        let event: Value = serde_json::from_str(&text)?;
        let kind = event["@type"].as_str().unwrap_or("unknown").to_owned();
        *counts.entry(kind.clone()).or_default() += 1;

        match kind.as_str() {
            "updateOption" if event["name"] == "version" => {
                println!("TDLib version: {}", event["value"]["value"]);
            }
            "error" => {
                // TDLib error texts describe the failure, not user data.
                eprintln!("TDLib error {}: {}", event["code"], event["message"]);
            }
            "updateAuthorizationState" => {
                let state = event["authorization_state"]["@type"]
                    .as_str()
                    .unwrap_or("unknown");
                println!("authorization: {state}");
                match state {
                    "authorizationStateWaitTdlibParameters" => send(
                        client,
                        &json!({
                            "@type": "setTdlibParameters",
                            "database_directory": data_dir.to_string_lossy(),
                            "database_encryption_key":
                                base64::engine::general_purpose::STANDARD.encode(&key),
                            "use_file_database": true,
                            "use_chat_info_database": true,
                            "use_message_database": true,
                            "use_secret_chats": false,
                            "api_id": api_id,
                            "api_hash": api_hash,
                            "system_language_code": "en",
                            "device_model": "Desktop",
                            "application_version": "0.1.0-probe",
                        }),
                    )?,
                    "authorizationStateWaitPhoneNumber" | "authorizationStateWaitCode"
                        if args.qr =>
                    {
                        request_qr(client)?;
                    }
                    "authorizationStateWaitOtherDeviceConfirmation" => {
                        let link = event["authorization_state"]["link"].as_str().unwrap_or("");
                        print_qr(link)?;
                    }
                    "authorizationStateWaitPhoneNumber" => {
                        let phone = prompt_visible("Phone number (international format): ")?;
                        send(
                            client,
                            &json!({"@type": "setAuthenticationPhoneNumber", "phone_number": phone}),
                        )?;
                    }
                    "authorizationStateWaitCode" => {
                        // Telegram usually delivers the code as a message to an already
                        // signed-in app, not by SMS, so say where to look for it.
                        let info = &event["authorization_state"]["code_info"];
                        let delivery = match info["type"]["@type"].as_str().unwrap_or("unknown") {
                            "authenticationCodeTypeTelegramMessage" => {
                                "a message in the \"Telegram\" service chat of an app where you are already signed in"
                            }
                            "authenticationCodeTypeSms" => "SMS",
                            "authenticationCodeTypeCall" => "a phone call",
                            "authenticationCodeTypeFlashCall"
                            | "authenticationCodeTypeMissedCall" => "a missed call",
                            "authenticationCodeTypeFragment" => "fragment.com",
                            other => other,
                        };
                        println!("The code was sent via: {delivery}");
                        let can_resend = !info["next_type"].is_null();
                        if can_resend {
                            println!(
                                "Press Enter without typing a code to request it another way."
                            );
                        }
                        let code = prompt_visible("Login code: ")?;
                        if code.trim().is_empty() && can_resend {
                            send(client, &json!({"@type": "resendAuthenticationCode"}))?;
                        } else {
                            send(
                                client,
                                &json!({"@type": "checkAuthenticationCode", "code": code.trim()}),
                            )?;
                        }
                    }
                    "authorizationStateWaitPassword" => {
                        let password = prompt_visible("Two-step password (shown as you type): ")?;
                        send(
                            client,
                            &json!({"@type": "checkAuthenticationPassword", "password": password}),
                        )?;
                    }
                    "authorizationStateReady" => {
                        ready_at = Some(Instant::now());
                        println!(
                            "READY after {:.1} s; staying up for {} s. Measure with:",
                            started.elapsed().as_secs_f32(),
                            args.run.as_secs()
                        );
                        println!("  .\\scripts\\measure.ps1 -Name tdlib-probe -DurationSec 600");
                        send(
                            client,
                            &json!({
                                "@type": "loadChats",
                                "chat_list": {"@type": "chatListMain"},
                                "limit": 100,
                            }),
                        )?;
                    }
                    "authorizationStateClosed" => break,
                    "authorizationStateLoggingOut" | "authorizationStateClosing" => {}
                    other => {
                        return Err(format!(
                            "authorization step not supported by the probe: {other}"
                        )
                        .into());
                    }
                }
            }
            _ => {}
        }
    }

    println!(
        "received {} events, {:.1} MB of JSON in {:.0} s",
        counts.values().sum::<u64>(),
        bytes_received as f64 / (1024.0 * 1024.0),
        started.elapsed().as_secs_f32()
    );
    let mut by_count: Vec<_> = counts.into_iter().collect();
    by_count.sort_by(|a, b| b.1.cmp(&a.1));
    for (kind, count) in by_count.iter().take(15) {
        println!("  {count:>8}  {kind}");
    }
    Ok(())
}
