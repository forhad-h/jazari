//! Secrets jazari needs (the Telegram bot token, the paired chat, the Claude login token for
//! workers). On macOS they live in the login Keychain; anywhere else, and to override the
//! Keychain, in environment variables. They are never written to marsad's own files.

use std::process::{Command, Stdio};

pub const TELEGRAM_TOKEN: &str = "marsad-jazari-telegram-token";
pub const TELEGRAM_CHAT: &str = "marsad-jazari-telegram-chat";
#[allow(dead_code)] // read by the workers in the next phase
pub const CLAUDE_TOKEN: &str = "marsad-jazari-claude-token";

/// The environment variable that overrides a Keychain service:
/// `marsad-jazari-telegram-token` → `MARSAD_JAZARI_TELEGRAM_TOKEN`.
pub fn env_name(service: &str) -> String {
    service.to_ascii_uppercase().replace('-', "_")
}

/// The secret, from its environment variable or the Keychain. None when it is set in neither.
pub fn get(service: &str) -> Option<String> {
    if let Some(v) = std::env::var(env_name(service)).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty()) {
        return Some(v);
    }
    keychain_get(service)
}

#[cfg(target_os = "macos")]
fn keychain_get(service: &str) -> Option<String> {
    let out = Command::new("security").args(["find-generic-password", "-s", service, "-w"]).stderr(Stdio::null()).output().ok()?;
    let v = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (out.status.success() && !v.is_empty()).then_some(v)
}

#[cfg(not(target_os = "macos"))]
fn keychain_get(_service: &str) -> Option<String> {
    None
}

/// Stores a value that is not itself a credential (the paired chat id). Tokens are added by the
/// user with `security add-generic-password … -w`, so they never pass through a command line.
pub fn set(service: &str, value: &str) -> anyhow::Result<()> {
    if cfg!(target_os = "macos") {
        let user = std::env::var("USER").unwrap_or_else(|_| "marsad".into());
        let ok = Command::new("security")
            .args(["add-generic-password", "-U", "-a", &user, "-s", service, "-w", value])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?
            .success();
        anyhow::ensure!(ok, "couldn't save {service} in the Keychain");
        return Ok(());
    }
    anyhow::bail!("no Keychain on this system: set {}={value} in the environment that runs marsad", env_name(service))
}

/// Removes a value from the Keychain; a missing one is not an error.
pub fn delete(service: &str) -> anyhow::Result<()> {
    if cfg!(target_os = "macos") {
        let _ = Command::new("security").args(["delete-generic-password", "-s", service]).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }
    Ok(())
}

/// How to add a token by hand, so it never shows up in a shell history or a chat.
pub fn how_to_add(service: &str) -> String {
    if cfg!(target_os = "macos") {
        format!("run `security add-generic-password -a \"$USER\" -s {service} -w` in a terminal and paste it when asked")
    } else {
        format!("set {} in the environment that runs marsad", env_name(service))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_service_has_an_environment_override() {
        assert_eq!(env_name(TELEGRAM_TOKEN), "MARSAD_JAZARI_TELEGRAM_TOKEN");
        assert_eq!(env_name(CLAUDE_TOKEN), "MARSAD_JAZARI_CLAUDE_TOKEN");
        assert!(how_to_add(TELEGRAM_TOKEN).contains(if cfg!(target_os = "macos") { TELEGRAM_TOKEN } else { "MARSAD_JAZARI_TELEGRAM_TOKEN" }));
    }
}
