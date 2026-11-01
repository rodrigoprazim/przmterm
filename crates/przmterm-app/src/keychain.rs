//! Saved passwords in the OS keychain: Keychain on macOS, Credential Manager
//! on Windows, the Secret Service (GNOME Keyring, KWallet) on Linux.

use przmterm_transport::ssh::PasswordStore;

const SERVICE: &str = "PrzmTerm";

pub struct Keychain;

impl PasswordStore for Keychain {
    fn get(&self, account: &str) -> Option<String> {
        keyring::Entry::new(SERVICE, account)
            .ok()?
            .get_password()
            .ok()
    }

    fn set(&self, account: &str, password: &str) -> Result<(), String> {
        keyring::Entry::new(SERVICE, account)
            .and_then(|e| e.set_password(password))
            .map_err(|e| e.to_string())
    }

    fn delete(&self, account: &str) {
        if let Ok(entry) = keyring::Entry::new(SERVICE, account) {
            let _ = entry.delete_credential();
        }
    }
}
