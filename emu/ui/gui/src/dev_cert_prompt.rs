//! Prompts the user (via a native Windows message box) to trust the shared self-signed dev
//! TLS certificate used by the AMQPS/HTTPS listeners, instead of silently attempting it (or
//! silently failing) in the background - see `emu-dev-cert` for why this certificate exists
//! and what it's for.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

use windows_sys::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, IDYES, MB_ICONINFORMATION, MB_ICONQUESTION, MB_OK, MB_YESNO,
};

fn to_wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

fn message_box(title: &str, message: &str, flags: u32) -> i32 {
    let title = to_wide(title);
    let message = to_wide(message);
    // Safe: both buffers are valid, null-terminated UTF-16 strings that outlive the call;
    // a null window handle just means the message box isn't owned by a specific window.
    unsafe { MessageBoxW(std::ptr::null_mut(), message.as_ptr(), title.as_ptr(), flags) }
}

fn ask_yes_no(title: &str, message: &str) -> bool {
    message_box(title, message, MB_YESNO | MB_ICONQUESTION) == IDYES
}

fn show_info(title: &str, message: &str) {
    message_box(title, message, MB_OK | MB_ICONINFORMATION);
}

/// If `dev_cert` isn't trusted yet, asks the user (once) whether to trust it now, and runs
/// the trust operation if they agree. Safe to call once at startup regardless of how many
/// TLS-capable resources (Service Bus AMQPS, Storage Blob HTTPS) end up being used - each of
/// them loads the same persisted certificate via `emu_dev_cert::load_or_generate()`, so
/// trusting it here covers all of them.
pub fn ensure_trusted(dev_cert: &emu_dev_cert::DevCertificate) {
    if dev_cert.is_trusted() {
        return;
    }

    let message = format!(
        "AzLocalDev uses a local, self-signed TLS certificate for its Managed-Identity-style \
         (AMQPS/HTTPS) endpoints, at:\n\n{}\n\n\
         Trusting it lets apps using DefaultAzureCredential/TokenCredential connect without \
         TLS errors. Trust it now?",
        dev_cert.cert_path.display()
    );

    if !ask_yes_no("AzLocalDev - Trust dev certificate?", &message) {
        return;
    }

    match dev_cert.trust() {
        Ok(()) => show_info("AzLocalDev", "Certificate trusted successfully."),
        Err(err) => show_info(
            "AzLocalDev",
            &format!(
                "Couldn't trust it automatically ({err}).\n\n\
                 You can trust it manually by running this in an elevated prompt if needed:\n\n\
                 certutil -user -addstore Root \"{}\"",
                dev_cert.cert_path.display()
            ),
        ),
    }
}
