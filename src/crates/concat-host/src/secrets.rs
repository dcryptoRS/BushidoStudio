// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors
// SPDX-FileCopyrightText: 2026 Royalty Studio

//! The desktop's keyring (Royalty Studio fork): one secret kept in the
//! Secret Service - GNOME Keyring, KeePassXC, KWallet's bridge - over the
//! session bus, found again by its attributes. What the generating
//! services' keys are kept in when there is one.
//!
//! Only an unlocked keyring is used: unlocking asks for a password in a
//! dialog of the desktop's, and these calls are made on the window's
//! thread, which must not wait on a person. A login keyring is unlocked
//! when the session starts, which is the usual case; otherwise the caller
//! keeps its secret somewhere else.

use std::collections::HashMap;

use zbus::blocking::Connection;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

const BUS: &str = "org.freedesktop.secrets";
const SERVICE_PATH: &str = "/org/freedesktop/secrets";
const SERVICE: &str = "org.freedesktop.Secret.Service";
const COLLECTION: &str = "org.freedesktop.Secret.Collection";
const ITEM: &str = "org.freedesktop.Secret.Item";
/// The collection a desktop keeps a person's secrets in.
const DEFAULT: &str = "/org/freedesktop/secrets/aliases/default";

/// A secret as the protocol carries it: the session, its parameters, the
/// bytes and their type.
type Secret = (OwnedObjectPath, Vec<u8>, Vec<u8>, String);

/// An open line to the keyring.
struct Keyring {
    connection: Connection,
    session: OwnedObjectPath,
}

impl Keyring {
    /// The session bus's keyring, with a session that passes secrets
    /// as they are: the bus is this user's alone.
    fn open() -> Result<Keyring, String> {
        let connection = Connection::session().map_err(|error| error.to_string())?;
        let reply = connection
            .call_method(
                Some(BUS),
                SERVICE_PATH,
                Some(SERVICE),
                "OpenSession",
                &("plain", Value::from("")),
            )
            .map_err(|error| error.to_string())?;
        let (_, session): (OwnedValue, OwnedObjectPath) = reply
            .body()
            .deserialize()
            .map_err(|error| error.to_string())?;
        Ok(Keyring {
            connection,
            session,
        })
    }

    /// The unlocked items with these attributes, and whether any is locked.
    fn search(
        &self,
        attributes: &HashMap<&str, &str>,
    ) -> Result<(Vec<OwnedObjectPath>, bool), String> {
        let reply = self
            .connection
            .call_method(
                Some(BUS),
                SERVICE_PATH,
                Some(SERVICE),
                "SearchItems",
                &(attributes,),
            )
            .map_err(|error| error.to_string())?;
        let (unlocked, locked): (Vec<OwnedObjectPath>, Vec<OwnedObjectPath>) = reply
            .body()
            .deserialize()
            .map_err(|error| error.to_string())?;
        Ok((unlocked, !locked.is_empty()))
    }

    fn secret_of(&self, item: &OwnedObjectPath) -> Result<Vec<u8>, String> {
        let reply = self
            .connection
            .call_method(
                Some(BUS),
                item.as_str(),
                Some(ITEM),
                "GetSecret",
                &(&self.session,),
            )
            .map_err(|error| error.to_string())?;
        let (secret,): (Secret,) = reply
            .body()
            .deserialize()
            .map_err(|error| error.to_string())?;
        Ok(secret.2)
    }
}

/// The secret kept under `attributes`; `Ok(None)` when the keyring has
/// none, and an error when there is no keyring to ask or it is locked.
pub fn load(attributes: &HashMap<&str, &str>) -> Result<Option<Vec<u8>>, String> {
    let keyring = Keyring::open()?;
    let (unlocked, locked) = keyring.search(attributes)?;
    match unlocked.first() {
        Some(item) => keyring.secret_of(item).map(Some),
        None if locked => Err("the keyring is locked".to_owned()),
        None => Ok(None),
    }
}

/// Keeps `secret` under `attributes` in the default collection, in place
/// of what was there; an error when there is no unlocked keyring.
pub fn store(label: &str, attributes: &HashMap<&str, &str>, secret: &[u8]) -> Result<(), String> {
    let keyring = Keyring::open()?;
    let mut properties: HashMap<&str, Value> = HashMap::new();
    properties.insert("org.freedesktop.Secret.Item.Label", Value::from(label));
    properties.insert(
        "org.freedesktop.Secret.Item.Attributes",
        Value::from(attributes.clone()),
    );
    let secret: (&OwnedObjectPath, Vec<u8>, Vec<u8>, &str) = (
        &keyring.session,
        Vec::new(),
        secret.to_vec(),
        "application/json",
    );
    let reply = keyring
        .connection
        .call_method(
            Some(BUS),
            DEFAULT,
            Some(COLLECTION),
            "CreateItem",
            &(properties, secret, true),
        )
        .map_err(|error| error.to_string())?;
    let (_, prompt): (OwnedObjectPath, OwnedObjectPath) = reply
        .body()
        .deserialize()
        .map_err(|error| error.to_string())?;
    // A prompt is the collection asking to be unlocked first; it is not
    // answered here (see the module's note), so nothing was kept.
    if prompt.as_str() != "/" {
        return Err("the keyring is locked".to_owned());
    }
    Ok(())
}

/// Takes every unlocked item under `attributes` out of the keyring.
pub fn forget(attributes: &HashMap<&str, &str>) -> Result<(), String> {
    let keyring = Keyring::open()?;
    let (unlocked, _) = keyring.search(attributes)?;
    for item in unlocked {
        keyring
            .connection
            .call_method(Some(BUS), item.as_str(), Some(ITEM), "Delete", &())
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Against the desktop's own keyring, so not run unless asked for:
    /// `cargo test -p concat-host secrets -- --ignored`. It leaves nothing
    /// behind.
    #[test]
    #[ignore = "writes into the desktop's keyring"]
    fn the_keyring_keeps_a_secret_and_gives_it_back() {
        let attributes = HashMap::from([
            ("application", "app.concat.editor.test"),
            ("kind", "round-trip"),
        ]);
        store("Concat test", &attributes, b"{\"gemini\":\"x\"}").expect("kept");
        store("Concat test", &attributes, b"{\"gemini\":\"y\"}").expect("replaced");
        assert_eq!(
            load(&attributes).expect("read"),
            Some(b"{\"gemini\":\"y\"}".to_vec())
        );
        forget(&attributes).expect("forgotten");
        assert_eq!(load(&attributes).expect("read"), None);
    }
}
