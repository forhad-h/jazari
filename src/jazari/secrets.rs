//! Secrets jazari needs (the Telegram bot token, the paired chat, the Claude login token for
//! workers). On macOS they live in the login Keychain; anywhere else, and to override the
//! Keychain, in environment variables. They are never written to jazari's own files.

use std::process::{Command, Stdio};

pub const TELEGRAM_TOKEN: &str = "jazari-telegram-token";
pub const TELEGRAM_CHAT: &str = "jazari-telegram-chat";
pub const CLAUDE_TOKEN: &str = "jazari-claude-token";

/// The environment variable that overrides a Keychain service:
/// `jazari-telegram-token` → `JAZARI_TELEGRAM_TOKEN`.
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

/// Whether `security -i` can take a value as one plain word: letters, digits and `:_-.`.
fn plain_word(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '_' | '-' | '.'))
}

/// The command for `security -i` that stores `value`. It goes on stdin, so the value never
/// appears in the process list.
fn add_command(user: &str, service: &str, value: &str) -> anyhow::Result<String> {
    anyhow::ensure!(plain_word(service) && plain_word(value), "{service} can't be stored: it may hold only letters, digits and :_-.");
    let user = if plain_word(user) { user } else { "jazari" };
    Ok(format!("add-generic-password -U -a {user} -s {service} -w {value}\n"))
}

/// Stores a value in the Keychain through `security -i` on stdin, then reads it back.
pub fn set(service: &str, value: &str) -> anyhow::Result<()> {
    if cfg!(target_os = "macos") {
        let user = std::env::var("USER").unwrap_or_else(|_| "jazari".into());
        let command = add_command(&user, service, value)?;
        let mut child = Command::new("security").arg("-i").stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            std::io::Write::write_all(&mut stdin, command.as_bytes())?;
        }
        let ok = child.wait()?.success() && keychain_get(service).as_deref() == Some(value);
        anyhow::ensure!(ok, "couldn't save {service} in the Keychain");
        return Ok(());
    }
    anyhow::bail!("no Keychain on this system: set {} in the environment that runs jazari", env_name(service))
}

/// Removes a value from the Keychain. Returns whether there was one to remove; a missing one, or
/// a system without a Keychain, is not an error.
pub fn delete(service: &str) -> anyhow::Result<bool> {
    if !cfg!(target_os = "macos") {
        return Ok(false);
    }
    let removed = Command::new("security").args(["delete-generic-password", "-s", service]).stdout(Stdio::null()).stderr(Stdio::null()).status()?.success();
    Ok(removed)
}

/// How to add a token by hand, so it never shows up in a shell history or a chat.
pub fn how_to_add(service: &str) -> String {
    if cfg!(target_os = "macos") {
        format!("run `security add-generic-password -a \"$USER\" -s {service} -w` in a terminal and paste it when asked")
    } else {
        format!("set {} in the environment that runs jazari", env_name(service))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_keychain_command_takes_only_plain_words() {
        assert_eq!(add_command("forhad", TELEGRAM_CHAT, "-1001234").unwrap(), "add-generic-password -U -a forhad -s jazari-telegram-chat -w -1001234\n");
        assert_eq!(add_command("a b", TELEGRAM_CHAT, "42").unwrap(), "add-generic-password -U -a jazari -s jazari-telegram-chat -w 42\n");
        for bad in ["", "two words", "x\nadd-generic-password -s other -w y", "q\"uote"] {
            assert!(add_command("u", TELEGRAM_CHAT, bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn each_service_has_an_environment_override() {
        assert_eq!(env_name(TELEGRAM_TOKEN), "JAZARI_TELEGRAM_TOKEN");
        assert_eq!(env_name(CLAUDE_TOKEN), "JAZARI_CLAUDE_TOKEN");
        assert!(how_to_add(TELEGRAM_TOKEN).contains(if cfg!(target_os = "macos") { TELEGRAM_TOKEN } else { "JAZARI_TELEGRAM_TOKEN" }));
    }
}
