//! A check that the user is at the Mac before the dashboard allows a session's tool call or answers
//! its question: Touch ID, or the Mac password. A session can type into the dashboard (for example
//! with `tmux send-keys`), but it cannot pass this check. A deny needs no check, because a deny is
//! always safe.

/// Whether this machine can check that the user is present. Where it can't, the dashboard offers
/// only deny.
pub fn available() -> bool {
    imp::available()
}

/// Asks for Touch ID or the password, showing `reason`. Blocks until the user answers, so run it
/// on a thread. True only when the check passed; a cancel, a failure or a timeout is false.
pub fn confirm(reason: &str) -> bool {
    imp::confirm(reason)
}

#[cfg(target_os = "macos")]
mod imp {
    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2_foundation::{NSError, NSString};
    use objc2_local_authentication::{LAContext, LAPolicy};

    /// The longest the dashboard waits for the user to confirm.
    const WAIT: std::time::Duration = std::time::Duration::from_secs(120);

    pub fn available() -> bool {
        // SAFETY: a new context, asked whether the owner's authentication can run.
        unsafe { LAContext::new().canEvaluatePolicy_error(LAPolicy::DeviceOwnerAuthentication).is_ok() }
    }

    pub fn confirm(reason: &str) -> bool {
        let (tx, rx) = std::sync::mpsc::channel();
        let reply = RcBlock::new(move |ok: Bool, _: *mut NSError| {
            let _ = tx.send(ok.as_bool());
        });
        // SAFETY: the context and the reason live until the reply arrives or the wait ends.
        let context = unsafe { LAContext::new() };
        let reason = NSString::from_str(reason);
        unsafe { context.evaluatePolicy_localizedReason_reply(LAPolicy::DeviceOwnerAuthentication, &reason, &reply) };
        let ok = rx.recv_timeout(WAIT).unwrap_or(false);
        if !ok {
            // SAFETY: ends a check that is still on screen after a timeout.
            unsafe { context.invalidate() };
        }
        ok
    }
}

/// Linux and Windows have no standard check of this kind yet.
#[cfg(not(target_os = "macos"))]
mod imp {
    pub fn available() -> bool {
        false
    }

    pub fn confirm(_: &str) -> bool {
        false
    }
}
