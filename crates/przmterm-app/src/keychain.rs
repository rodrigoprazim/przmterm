//! Saved passwords in the OS keychain: Keychain on macOS, Credential Manager
//! on Windows, the Secret Service (GNOME Keyring, KWallet) on Linux.
//!
//! On macOS, builds that are not signed count as a new app each time they
//! change, so the system asks for the login keychain password (the Mac
//! account password) and "Always Allow" again after every rebuild.

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
        let entry = keyring::Entry::new(SERVICE, account).map_err(|e| e.to_string())?;
        match entry.set_password(password) {
            Ok(()) => Ok(()),
            // macOS: an item saved by an earlier (differently signed) build
            // cannot be updated in place. Replace it.
            Err(e) if e.to_string().contains("already exists") => {
                let _ = entry.delete_credential();
                entry.set_password(password).map_err(|e| e.to_string())
            }
            Err(e) => Err(e.to_string()),
        }
    }

    fn delete(&self, account: &str) {
        if let Ok(entry) = keyring::Entry::new(SERVICE, account) {
            let _ = entry.delete_credential();
        }
    }
}
