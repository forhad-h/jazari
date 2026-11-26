//! jazari: the part of marsad that will run an AI software company. This first part connects
//! marsad to the founder's Telegram: pairing, the bridge that takes commands and forwards sessions'
//! `--attention` notes, and `notify` for scripts.

mod bridge;
pub mod secrets;
pub mod telegram;

use clap::Subcommand;
use std::path::PathBuf;
use telegram::{Bot, Curl};

#[derive(Subcommand)]
pub enum JazariCmd {
    /// Connect your Telegram bot: pair a chat, send a test message, check or remove the pairing.
    #[command(subcommand)]
    Telegram(TelegramCmd),
    /// Take commands from the paired Telegram chat and forward sessions' --attention notes (runs until stopped).
    Bridge,
    /// Send a message to the paired Telegram chat (credentials in it are redacted).
    Notify {
        /// The message; read from stdin when left out.
        message: Vec<String>,
    },
}

#[derive(Subcommand)]
pub enum TelegramCmd {
    /// Show a one-time code and wait for it in a private chat with the bot; that chat becomes the only one obeyed.
    Pair,
    /// Send a test message to the paired chat.
    Test,
    /// Whether the bot token and a paired chat are set.
    Status,
    /// Forget the paired chat.
    Unpair,
}

/// jazari's own files: `~/.marsad/jazari`.
pub fn dir() -> PathBuf {
    crate::data_dir().join("jazari")
}

fn bot() -> anyhow::Result<Bot<Curl>> {
    let token = secrets::get(secrets::TELEGRAM_TOKEN)
        .ok_or_else(|| anyhow::anyhow!("no Telegram bot token: {}", secrets::how_to_add(secrets::TELEGRAM_TOKEN)))?;
    Ok(Bot { transport: Curl::new(token) })
}

fn chat() -> anyhow::Result<i64> {
    secrets::get(secrets::TELEGRAM_CHAT)
        .and_then(|c| c.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("no Telegram chat is paired; run `marsad jazari telegram pair`"))
}

pub fn run(cmd: JazariCmd) -> anyhow::Result<()> {
    match cmd {
        JazariCmd::Telegram(TelegramCmd::Pair) => bridge::pair(&bot()?, &dir()),
        JazariCmd::Telegram(TelegramCmd::Test) => {
            bot()?.send(chat()?, "Test message from jazari: the bridge can reach you.")?;
            println!("Sent.");
            Ok(())
        }
        JazariCmd::Telegram(TelegramCmd::Status) => {
            let token = secrets::get(secrets::TELEGRAM_TOKEN).is_some();
            println!("Bot token: {}", if token { "set" } else { "missing" });
            if token {
                println!("Bot: @{}", bot()?.username()?);
            }
            println!("Paired chat: {}", if chat().is_ok() { "yes" } else { "no (`marsad jazari telegram pair`)" });
            Ok(())
        }
        JazariCmd::Telegram(TelegramCmd::Unpair) => {
            secrets::delete(secrets::TELEGRAM_CHAT)?;
            println!("Unpaired: no chat can control jazari until you pair again.");
            Ok(())
        }
        JazariCmd::Bridge => bridge::run(&bot()?, chat()?, &dir()),
        JazariCmd::Notify { message } => {
            let mut text = message.join(" ");
            if text.trim().is_empty() {
                std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)?;
            }
            anyhow::ensure!(!text.trim().is_empty(), "nothing to send");
            bot()?.send(chat()?, text.trim())
        }
    }
}
