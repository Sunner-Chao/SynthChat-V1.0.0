#![cfg(target_os = "windows")]

use std::{env, error::Error, fs, io, path::Path, sync::Arc};

use keyring_core::{CredentialStore, Error as KeyringError};
use secrecy::{ExposeSecret, SecretString};
use synthchat_hermes_backend::{
    ProfileService,
    profiles::{CreateProfile, ProfileError},
};
use tempfile::TempDir;
use uuid::Uuid;
use zeroize::Zeroizing;

const OPT_IN_ENV: &str = "SYNTHCHAT_RUN_NATIVE_KEYCHAIN_TESTS";
const SECRET_SERVICE: &str = "cc.synthchat.v1.hermes.secrets";

struct NativeKeychainFixture {
    home: TempDir,
    profile_id: String,
    idempotency_key: String,
    secret_name: String,
}

impl NativeKeychainFixture {
    fn new() -> io::Result<Self> {
        let profile_suffix = Uuid::new_v4().simple().to_string();
        let secret_suffix = Uuid::new_v4().simple().to_string().to_ascii_uppercase();

        Ok(Self {
            home: tempfile::Builder::new()
                .prefix("synthchat-native-keychain-")
                .tempdir()?,
            profile_id: format!("native-keychain-{profile_suffix}"),
            idempotency_key: format!("native-keychain-create-{profile_suffix}"),
            secret_name: format!("SYNTHCHAT_NATIVE_KEYCHAIN_{secret_suffix}"),
        })
    }

    fn service(&self) -> ProfileService {
        ProfileService::with_system_store(self.home.path().to_path_buf())
    }
}

impl Drop for NativeKeychainFixture {
    fn drop(&mut self) {
        let service = self.service();
        let _ = service.delete_secret(&self.profile_id, &self.secret_name);
        let _ = delete_native_credential(&self.profile_id, &self.secret_name);
        let _ = service.delete_profile(&self.profile_id);
    }
}

#[test]
#[ignore = "mutates the current user's Windows Credential Manager; set SYNTHCHAT_RUN_NATIVE_KEYCHAIN_TESTS=1 explicitly"]
fn windows_credential_manager_round_trip_is_persistent_and_disk_safe() -> Result<(), Box<dyn Error>>
{
    require_explicit_opt_in();

    let fixture = NativeKeychainFixture::new()?;
    let secret = SecretString::from(format!(
        "synthchat-native-keychain-value-{}-{}",
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple()
    ));

    let service = fixture.service();
    service.create_profile(
        &CreateProfile {
            id: fixture.profile_id.clone(),
            display_name: "Native Windows keychain integration".to_owned(),
            clone_from_profile_id: None,
        },
        &fixture.idempotency_key,
    )?;
    assert!(!native_credential_is_configured(
        &fixture.profile_id,
        &fixture.secret_name
    )?);

    let put_status = service.put_secret(&fixture.profile_id, &fixture.secret_name, &secret)?;
    assert!(put_status.configured);
    assert_eq!(put_status.storage, "osKeychain");
    assert!(put_status.updated_at.is_some());
    assert!(secret_status_is_configured(
        &service,
        &fixture.profile_id,
        &fixture.secret_name
    )?);
    assert!(native_credential_is_configured(
        &fixture.profile_id,
        &fixture.secret_name
    )?);
    assert!(native_credential_matches(
        &fixture.profile_id,
        &fixture.secret_name,
        secret.expose_secret().as_bytes()
    )?);
    assert_tree_does_not_contain(fixture.home.path(), secret.expose_secret().as_bytes())?;

    drop(service);
    let reconstructed = fixture.service();
    assert!(secret_status_is_configured(
        &reconstructed,
        &fixture.profile_id,
        &fixture.secret_name
    )?);
    assert!(native_credential_matches(
        &fixture.profile_id,
        &fixture.secret_name,
        secret.expose_secret().as_bytes()
    )?);
    assert_tree_does_not_contain(fixture.home.path(), secret.expose_secret().as_bytes())?;

    reconstructed.delete_secret(&fixture.profile_id, &fixture.secret_name)?;
    drop(reconstructed);
    let after_delete = fixture.service();
    assert!(!secret_status_is_configured(
        &after_delete,
        &fixture.profile_id,
        &fixture.secret_name
    )?);
    assert!(!native_credential_is_configured(
        &fixture.profile_id,
        &fixture.secret_name
    )?);
    assert_tree_does_not_contain(fixture.home.path(), secret.expose_secret().as_bytes())?;

    Ok(())
}

fn require_explicit_opt_in() {
    assert_eq!(
        env::var(OPT_IN_ENV).as_deref(),
        Ok("1"),
        "set {OPT_IN_ENV}=1 in addition to selecting the ignored native keychain test"
    );
}

fn secret_status_is_configured(
    service: &ProfileService,
    profile_id: &str,
    secret_name: &str,
) -> Result<bool, ProfileError> {
    Ok(service
        .list_secret_statuses(profile_id)?
        .into_iter()
        .find(|status| status.name == secret_name)
        .is_some_and(|status| status.configured))
}

fn native_credential_is_configured(
    profile_id: &str,
    secret_name: &str,
) -> Result<bool, KeyringError> {
    let store: Arc<CredentialStore> = windows_native_keyring_store::Store::new()?;
    let entry = store.build(SECRET_SERVICE, &format!("{profile_id}:{secret_name}"), None)?;
    match entry.get_secret() {
        Ok(value) => {
            let _secret = Zeroizing::new(value);
            Ok(true)
        }
        Err(KeyringError::NoEntry) => Ok(false),
        Err(error) => Err(error),
    }
}

fn native_credential_matches(
    profile_id: &str,
    secret_name: &str,
    expected: &[u8],
) -> Result<bool, KeyringError> {
    let store: Arc<CredentialStore> = windows_native_keyring_store::Store::new()?;
    let entry = store.build(SECRET_SERVICE, &format!("{profile_id}:{secret_name}"), None)?;
    match entry.get_secret() {
        Ok(value) => {
            let secret = Zeroizing::new(value);
            Ok(secret.as_slice() == expected)
        }
        Err(KeyringError::NoEntry) => Ok(false),
        Err(error) => Err(error),
    }
}

fn delete_native_credential(profile_id: &str, secret_name: &str) -> Result<(), KeyringError> {
    let store: Arc<CredentialStore> = windows_native_keyring_store::Store::new()?;
    let entry = store.build(SECRET_SERVICE, &format!("{profile_id}:{secret_name}"), None)?;
    match entry.delete_credential() {
        Ok(()) | Err(KeyringError::NoEntry) => Ok(()),
        Err(error) => Err(error),
    }
}

fn assert_tree_does_not_contain(root: &Path, needle: &[u8]) -> io::Result<()> {
    let mut directories = vec![root.to_path_buf()];
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            let path = entry.path();
            if file_type.is_dir() {
                directories.push(path);
            } else if file_type.is_file() && file_contains(&path, needle)? {
                return Err(io::Error::other(format!(
                    "plaintext credential material was found in {}",
                    path.display()
                )));
            }
        }
    }
    Ok(())
}

fn file_contains(path: &Path, needle: &[u8]) -> io::Result<bool> {
    let bytes = fs::read(path)?;
    Ok(bytes
        .windows(needle.len())
        .any(|candidate| candidate == needle))
}
