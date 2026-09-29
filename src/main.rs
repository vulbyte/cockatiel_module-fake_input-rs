//! Cockatiel fake-input module.
//!
//! Viewers spend points to trigger a fake keyboard/mouse input on the
//! STREAMER'S OWN machine — a way for chat to gently troll the streamer by
//! sending a harmless simulated key press or mouse click. The streamer opts in
//! by enabling this module and defines WHICH inputs viewers may trigger (a
//! whitelist: name -> key/button combo), so nothing unlisted can ever be fired.
//!
//! Command: `!input <name>` (e.g. `!input left-click`, `!input hold-w`,
//! `!input jump`). The module checks the viewer can afford the module's price,
//! deducts it from their CURRENT score, and performs the input locally via
//! `enigo`. On macOS the process needs Accessibility permission (grant once in
//! System Settings) for synthetic events to take effect.
//!
//! Only the input NAMES in `config.json`'s `inputs` map are accepted; the
//! module never parses raw keys from chat, so a viewer cannot inject an
//! arbitrary key.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use cockatiel_client::proto::container::Payload;
use cockatiel_client::proto::*;
use cockatiel_client::CockatielClient;
use enigo::{Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings};
use futures_util::{SinkExt, StreamExt};
use prost::Message;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex as AsyncMutex;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing::{error, info, warn};
use tracing_subscriber::FmtSubscriber;

type WsWriteHalf = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    WsMessage,
>;

const COMMAND_NAME: &str = "input";

// ── config ────────────────────────────────────────────────────────────────

/// The module's editable settings, persisted under config.json's flat
/// `module_specific` object (the convention every module uses).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct Config {
    /// The command flag prefix (e.g. `!`).
    command_flag: String,
    /// How much score a viewer must spend to trigger an input. Deducted from
    /// their CURRENT score; the lifetime total is untouched.
    price: u64,
    /// The whitelist of inputs viewers may trigger: friendly name -> an input
    /// spec. Anything NOT listed here is refused, so chat can never fire an
    /// arbitrary key. e.g.
    ///   "left-click": {"kind":"mouse","button":"left"}
    ///   "hold-w":     {"kind":"key","char":"w","hold_ms":1500}
    ///   "jump":       {"kind":"key","char":" "}
    inputs: HashMap<String, InputSpec>,
    /// Reconnect backoff bounds (seconds).
    reconnect_base_secs: u64,
    reconnect_max_secs: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            command_flag: "!".into(),
            price: 10_000,
            inputs: HashMap::new(),
            reconnect_base_secs: 1,
            reconnect_max_secs: 30,
        }
    }
}

/// One whitelisted input action.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
enum InputSpec {
    /// A keyboard key press (optionally held for `hold_ms`).
    #[serde(rename = "key")]
    Key {
        /// The character/letter to press (single char, e.g. "w" or " ").
        char: String,
        /// How long to hold the key (ms). 0/absent = a quick tap (click).
        #[serde(default)]
        hold_ms: u64,
    },
    /// A mouse button click.
    #[serde(rename = "mouse")]
    Mouse {
        /// "left" | "right" | "middle".
        button: String,
    },
    /// A mouse move by a relative offset.
    #[serde(rename = "mousemove")]
    MouseMove {
        /// Relative x offset (positive = right).
        x: i32,
        /// Relative y offset (positive = down).
        y: i32,
    },
}

fn load_config(path: &Path) -> Config {
    let data = std::fs::read_to_string(path).unwrap_or_default();
    let root: serde_json::Value =
        serde_json::from_str(&data).unwrap_or_else(|_| serde_json::json!({}));
    let specific = root
        .get("module_specific")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    serde_json::from_value(specific).unwrap_or_default()
}

// ── the input simulation ──────────────────────────────────────────────────

fn enigo_settings() -> Settings {
    // enigo 0.2 uses a Settings builder; defaults work on macOS/Linux/Windows.
    Settings::default()
}

/// Perform a whitelisted input via enigo. Returns Ok(()) on success.
fn simulate(spec: &InputSpec) -> Result<(), String> {
    let mut enigo = Enigo::new(&enigo_settings()).map_err(|e| format!("enigo init: {e}"))?;
    match spec {
        InputSpec::Key { char, hold_ms } => {
            let chars: Vec<char> = char.chars().collect();
            let c = chars.first().copied().ok_or("empty key char")?;
            if *hold_ms > 0 {
                enigo
                    .key(Key::Unicode(c), Direction::Press)
                    .map_err(|e| format!("key press: {e}"))?;
                std::thread::sleep(Duration::from_millis(*hold_ms));
                enigo
                    .key(Key::Unicode(c), Direction::Release)
                    .map_err(|e| format!("key release: {e}"))?;
            } else {
                enigo
                    .key(Key::Unicode(c), Direction::Click)
                    .map_err(|e| format!("key click: {e}"))?;
            }
        }
        InputSpec::Mouse { button } => {
            let b = match button.as_str() {
                "right" => Button::Right,
                "middle" => Button::Middle,
                _ => Button::Left,
            };
            enigo
                .button(b, Direction::Click)
                .map_err(|e| format!("mouse click: {e}"))?;
        }
        InputSpec::MouseMove { x, y } => {
            enigo
                .move_mouse(*x, *y, Coordinate::Rel)
                .map_err(|e| format!("mouse move: {e}"))?;
        }
    }
    Ok(())
}

// ── connection / message loop ─────────────────────────────────────────────

#[derive(Clone)]
struct Session {
    auth_token: String,
    module_name: String,
    instance_uuid7: String,
}

async fn send_container(write: &Arc<AsyncMutex<WsWriteHalf>>, container: Container) {
    let mut buf = Vec::new();
    if container.encode(&mut buf).is_ok() {
        let mut w = write.lock().await;
        let _ = w.send(WsMessage::Binary(buf)).await;
    }
}

/// Post a chat message to the platform the command came from.
async fn send_to_chat(write: &Arc<AsyncMutex<WsWriteHalf>>, s: &Session, msg: String, chat: &ChatMessage) {
    let Some(ud) = &chat.user_data else { return };
    let container = Container {
        version: 1,
        auth_token: s.auth_token.clone(),
        module_name: s.module_name.clone(),
        module_instance_uuid7: s.instance_uuid7.clone(),
        payload: Some(Payload::SendToPlatforms(SendToPlatforms {
            msg,
            level: 0,
            module_uuid7: s.instance_uuid7.clone(),
            pid: String::new(),
            platform: chat.platform.clone(),
            actor_platform: chat.platform.clone(),
            actor_handle: ud.username.clone(),
            actor_uuid7: chat.user_uuid7.clone(),
            channel_id: chat.channel_id.clone(),
        })),
    };
    send_container(write, container).await;
}

/// Parse `!input <name>` from the raw message: everything after the command
/// token, trimmed. e.g. `!input left-click` -> `left-click`.
fn extract_input_name(raw: &str, flag: &str) -> Option<String> {
    let trimmed = raw.trim();
    let lower = trimmed.to_ascii_lowercase();
    let token = format!("{}{}", flag.to_ascii_lowercase(), COMMAND_NAME);
    if !lower.starts_with(&token) {
        return None;
    }
    let rest = trimmed[token.len()..].trim();
    if rest.is_empty() {
        return None;
    }
    // Require a separator after the command token so `!inputfoo` isn't a command.
    if !trimmed[token.len()..].starts_with(char::is_whitespace) {
        return None;
    }
    Some(rest.to_string())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(tracing::Level::INFO)
        .with_writer(std::io::stderr)
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .map_err(|e| format!("tracing init: {e}"))?;

    let config = load_config(Path::new("config.json"));

    let cfg = config.clone();
    tokio::spawn(async move {
        session_loop(&cfg).await;
    });

    loop {
        tokio::time::sleep(Duration::from_secs(3600)).await;
    }
}

async fn session_loop(config: &Config) {
    let mut backoff = config.reconnect_base_secs;
    loop {
        match run_session(config).await {
            Ok(()) => {}
            Err(e) => error!("session error: {e}"),
        }
        warn!("engine disconnected — reconnecting in {backoff}s");
        tokio::time::sleep(Duration::from_secs(backoff)).await;
        backoff = (backoff * 2).min(config.reconnect_max_secs.max(1));
    }
}

async fn run_session(config: &Config) -> Result<(), Box<dyn std::error::Error>> {
    let client = CockatielClient::connect("config.json").await?;
    let (write, read) = client.stream.split();
    let write_shared: Arc<AsyncMutex<WsWriteHalf>> = Arc::new(AsyncMutex::new(write));
    let session = Session {
        auth_token: client.auth_token.clone(),
        module_name: client.config.module_name.clone(),
        instance_uuid7: client.instance_uuid7.clone(),
    };

    // Register the !input command so the engine routes `!input <name>` to us.
    let commands = Container {
        version: 1,
        auth_token: session.auth_token.clone(),
        module_name: session.module_name.clone(),
        module_instance_uuid7: session.instance_uuid7.clone(),
        payload: Some(Payload::CommandsPayload(Commands {
            commands: vec![Command {
                command_name: COMMAND_NAME.to_string(),
                command_flag: config.command_flag.clone(),
                command_description: "spend points to trigger a fake input on the streamer's machine (e.g. !input left-click, !input jump)".to_string(),
                command_flags: vec![],
            }],
            alert_on_unknown_command: false,
        })),
    };
    send_container(&write_shared, commands).await;

    let flag = config.command_flag.clone();
    let inputs = config.inputs.clone();

    let s = session.clone();
    let mut read = read;
    loop {
        let Some(msg) = read.next().await else { break };
        let data = match msg {
            Ok(WsMessage::Binary(d)) => d,
            Ok(WsMessage::Close(_)) => break,
            Ok(_) => continue,
            Err(e) => {
                warn!("ws error: {e}");
                break;
            }
        };
        let Ok(container) = Container::decode(data.as_ref()) else {
            continue;
        };

        match container.payload {
            Some(Payload::AuthVerify(_)) => {
                let reply = Container {
                    version: 1,
                    auth_token: s.auth_token.clone(),
                    module_name: s.module_name.clone(),
                    module_instance_uuid7: s.instance_uuid7.clone(),
                    payload: Some(Payload::AuthVerify(AuthVerify {
                        cur_auth: s.auth_token.clone(),
                    })),
                };
                send_container(&write_shared, reply).await;
            }
            Some(Payload::MessagePreProcess(pre)) => {
                let MessagePreProcess { message_uuid7: uuid, raw_message, audio, audio_type } = pre;
                // ACK the stage on EVERY path so the pipeline never stalls.
                let ack = Container {
                    version: 1,
                    auth_token: s.auth_token.clone(),
                    module_name: s.module_name.clone(),
                    module_instance_uuid7: s.instance_uuid7.clone(),
                    payload: Some(Payload::MessagePreProcess(MessagePreProcess {
                        message_uuid7: uuid,
                        raw_message: raw_message.clone(),
                        audio,
                        audio_type,
                    })),
                };
                send_container(&write_shared, ack).await;

                let Some(chat) = &raw_message else { continue };
                let Some(cmd) = &chat.command else { continue };
                if cmd.command_name != COMMAND_NAME {
                    continue;
                }
                let Some(name) = extract_input_name(&chat.raw_message, &flag) else { continue };

                // The ENGINE already gates this module: it skips the module
                // entirely when the viewer can't afford the manifest `price`
                // (and enforces `authority`/`min_rank`), so a message reaching
                // us means the viewer was already charged. This module must not
                // deduct a second time — it just performs the whitelisted input.

                // Whitelist check: refuse anything not configured.
                let Some(spec) = inputs.get(&name) else {
                    send_to_chat(
                        &write_shared,
                        &s,
                        format!("unknown input '{name}' — the streamer hasn't enabled that one"),
                        chat,
                    )
                    .await;
                    continue;
                };

                let handle = chat.user_data.as_ref().map(|u| u.username.clone()).unwrap_or_default();
                match simulate(spec) {
                    Ok(()) => {
                        info!("fired input '{name}' for {handle}");
                        send_to_chat(
                            &write_shared,
                            &s,
                            format!("{handle} triggered {name}!"),
                            chat,
                        ).await;
                    }
                    Err(e) => {
                        error!("input '{name}' failed: {e}");
                        send_to_chat(
                            &write_shared,
                            &s,
                            format!("{handle}: '{name}' could not be performed (see the streamer's log)"),
                            chat,
                        ).await;
                    }
                }
            }
            Some(Payload::MessageInProcess(process)) => {
                let ack = Container {
                    version: 1,
                    auth_token: s.auth_token.clone(),
                    module_name: s.module_name.clone(),
                    module_instance_uuid7: s.instance_uuid7.clone(),
                    payload: Some(Payload::MessageInProcess(MessageInProcess {
                        message_uuid7: process.message_uuid7,
                        raw_message: process.raw_message,
                        processed_message: process.processed_message,
                        abandon_message: process.abandon_message,
                        audio: process.audio,
                        audio_type: process.audio_type,
                    })),
                };
                send_container(&write_shared, ack).await;
            }
            _ => {}
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_input_name_parses_command() {
        assert_eq!(extract_input_name("!input left-click", "!"), Some("left-click".into()));
        assert_eq!(extract_input_name("  !input   hold-w  ", "!"), Some("hold-w".into()));
        assert_eq!(extract_input_name("!input jump", "!"), Some("jump".into()));
        assert_eq!(extract_input_name("!input", "!"), None, "no name");
        assert_eq!(extract_input_name("!inputfoo", "!"), None, "no separator");
        assert_eq!(extract_input_name("hello world", "!"), None, "not a command");
    }

    #[test]
    fn whitelist_controls_what_can_be_fired() {
        let mut cfg = Config::default();
        cfg.inputs.insert(
            "left-click".into(),
            InputSpec::Mouse { button: "left".into() },
        );
        assert!(cfg.inputs.contains_key("left-click"));
        assert!(!cfg.inputs.contains_key("rm -rf"), "chat can never name an unlisted input");
    }
}