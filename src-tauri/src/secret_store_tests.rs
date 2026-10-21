use super::*;

const PASSPHRASE: &str = "correct horse battery staple";

fn credential(store: &Arc<FileSecretStore>, user: &str) -> Box<Credential> {
    FileCredentialBuilder(store.clone())
        .build(None, "tabularis", user)
        .unwrap()
}

#[test]
fn round_trips_secrets_across_reopen_without_storing_plaintext() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("secrets.enc.json");
    let store = Arc::new(FileSecretStore::open(path.clone(), PASSPHRASE).unwrap());

    credential(&store, "conn-1:db").set_password("s3cret-value").unwrap();
    assert_eq!(
        credential(&store, "conn-1:db").get_password().unwrap(),
        "s3cret-value"
    );
    assert!(!fs::read_to_string(&path).unwrap().contains("s3cret-value"));

    let reopened = Arc::new(FileSecretStore::open(path, PASSPHRASE).unwrap());
    assert_eq!(
        credential(&reopened, "conn-1:db").get_password().unwrap(),
        "s3cret-value"
    );
}

#[test]
fn reports_missing_entries_as_no_entry() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(
        FileSecretStore::open(temp.path().join("secrets.enc.json"), PASSPHRASE).unwrap(),
    );
    let missing = credential(&store, "missing");

    assert!(matches!(missing.get_password(), Err(keyring::Error::NoEntry)));
    assert!(matches!(
        missing.delete_credential(),
        Err(keyring::Error::NoEntry)
    ));

    missing.set_password("value").unwrap();
    missing.delete_credential().unwrap();
    assert!(matches!(missing.get_password(), Err(keyring::Error::NoEntry)));
}

#[test]
fn keeps_targets_services_and_users_apart() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(
        FileSecretStore::open(temp.path().join("secrets.enc.json"), PASSPHRASE).unwrap(),
    );
    let builder = FileCredentialBuilder(store.clone());
    builder
        .build(None, "tabularis", "a")
        .unwrap()
        .set_password("one")
        .unwrap();
    builder
        .build(Some("other"), "tabularis", "a")
        .unwrap()
        .set_password("two")
        .unwrap();

    assert_eq!(credential(&store, "a").get_password().unwrap(), "one");
    assert!(matches!(
        builder.build(None, "other-service", "a").unwrap().get_password(),
        Err(keyring::Error::NoEntry)
    ));
}

#[test]
fn rejects_a_wrong_or_short_key() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("secrets.enc.json");
    FileSecretStore::open(path.clone(), PASSPHRASE).unwrap();

    let error = FileSecretStore::open(path.clone(), "a different passphrase")
        .err()
        .unwrap();
    assert!(error.contains("does not match"), "{error}");
    let error = FileSecretStore::open(path, "short").err().unwrap();
    assert!(error.contains("at least"), "{error}");
}

#[cfg(unix)]
#[test]
fn writes_the_store_privately() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("nested").join("secrets.enc.json");
    FileSecretStore::open(path.clone(), PASSPHRASE).unwrap();

    let mode = fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[test]
fn generates_and_reuses_a_key_when_none_is_configured() {
    let temp = tempfile::tempdir().unwrap();
    let first = generated_key(temp.path()).unwrap();
    let second = generated_key(temp.path()).unwrap();

    assert_eq!(first, second);
    assert!(first.chars().count() >= MIN_KEY_CHARS);
    let store = temp.path().join("secrets.enc.json");
    FileSecretStore::open(store, &first).unwrap();
}
