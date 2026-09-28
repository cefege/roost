//! The IndexedDB half of the browser vault: open-with-upgrade, read, `add`,
//! delete of the device-key slot. Owned by `platform::device_key`'s browser
//! vault; `wasm32` only, and it only makes web-sys calls — the schema and its
//! upgrade are planned in `schema`, and every decision about a result is the
//! lifecycle's. Ported from `apps/web/src/client/auth/web-key-storage.ts`.

use js_sys::{Function, Promise, Reflect};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast as _, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    CryptoKey, CryptoKeyPair, DomException, IdbDatabase, IdbObjectStore, IdbOpenDbRequest,
    IdbRequest, IdbTransaction, IdbTransactionMode,
};

use super::lifecycle::AddFailure;
use super::schema::{
    DATABASE_NAME, DATABASE_VERSION, DEVICE_KEY_SLOT, KEY_STORE_NAME, LEGACY_TRUST_STORE_NAME,
    plan_schema_upgrade,
};

/// The pair in the device-key slot, if there is one.
pub async fn read_current() -> Result<Option<CryptoKeyPair>, String> {
    let database = open_database().await?;
    let outcome = read_slot(&database).await;
    database.close();
    outcome
}

/// `add` — never `put` — the pair into the device-key slot.
pub async fn add_current(pair: &CryptoKeyPair) -> Result<(), AddFailure> {
    let database = open_database().await.map_err(|message| AddFailure {
        error_name: "OpenError".to_owned(),
        message,
    })?;
    let outcome = add_slot(&database, pair).await;
    database.close();
    outcome
}

/// Delete the device-key slot. Deleting an empty slot succeeds.
pub async fn delete_current() -> Result<(), String> {
    let database = open_database().await?;
    let outcome = delete_slot(&database).await;
    database.close();
    outcome
}

/// A JS value as one line of text: a DOMException's `name: message`, a thrown
/// string verbatim, anything else debug-printed.
pub fn describe_js(value: &JsValue) -> String {
    if let Some(exception) = value.dyn_ref::<DomException>() {
        return format!("{}: {}", exception.name(), exception.message());
    }
    value.as_string().unwrap_or_else(|| format!("{value:?}"))
}

/// A stored or generated key pair, checked field by field.
///
/// `CryptoKeyPair` is a dictionary, not a class, so there is nothing to
/// `instanceof`: the two halves are checked as `CryptoKey`s instead, and a
/// record that is anything else is refused rather than signed with.
pub fn key_pair_from(value: JsValue) -> Result<CryptoKeyPair, String> {
    for half in ["privateKey", "publicKey"] {
        let key = Reflect::get(&value, &JsValue::from_str(half)).map_err(|e| describe_js(&e))?;
        if !key.is_instance_of::<CryptoKey>() {
            return Err(format!("the stored device key has no {half}"));
        }
    }
    Ok(value.unchecked_into())
}

async fn open_database() -> Result<IdbDatabase, String> {
    let factory = web_sys::window()
        .ok_or("this document has no window")?
        .indexed_db()
        .map_err(|e| describe_js(&e))?
        .ok_or("this browser exposes no IndexedDB")?;
    let request = factory
        .open_with_u32(DATABASE_NAME, DATABASE_VERSION)
        .map_err(|e| describe_js(&e))?;
    let upgrading = request.clone();
    // Returning the error makes wasm-bindgen throw it inside the handler, which
    // aborts the version change and fails the open — v2's behaviour, because a
    // half-upgraded database is not one to read a key from.
    let on_upgrade =
        Closure::<dyn FnMut() -> Result<(), JsValue>>::new(move || upgrade_schema(&upgrading));
    request.set_onupgradeneeded(Some(on_upgrade.as_ref().unchecked_ref()));
    let settled = request_settled(&request).await;
    request.set_onupgradeneeded(None);
    drop(on_upgrade);
    settled?;
    request
        .result()
        .map_err(|e| describe_js(&e))?
        .dyn_into::<IdbDatabase>()
        .map_err(|_| "the IndexedDB open produced no database".to_owned())
}

fn upgrade_schema(request: &IdbOpenDbRequest) -> Result<(), JsValue> {
    let database: IdbDatabase = request.result()?.dyn_into()?;
    let names = database.object_store_names();
    let plan = plan_schema_upgrade(
        names.contains(KEY_STORE_NAME),
        names.contains(LEGACY_TRUST_STORE_NAME),
    );
    if plan.create_key_store {
        database.create_object_store(KEY_STORE_NAME)?;
    }
    if plan.delete_legacy_trust_store {
        database.delete_object_store(LEGACY_TRUST_STORE_NAME)?;
    }
    tracing::info!(
        target: "auth",
        created_key_store = plan.create_key_store,
        deleted_legacy_trust_store = plan.delete_legacy_trust_store,
        "auth.key_store_upgraded"
    );
    Ok(())
}

async fn read_slot(database: &IdbDatabase) -> Result<Option<CryptoKeyPair>, String> {
    let (_, store) = key_store(database, IdbTransactionMode::Readonly)?;
    let request = store
        .get(&JsValue::from_str(DEVICE_KEY_SLOT))
        .map_err(|e| describe_js(&e))?;
    request_settled(&request).await?;
    let value = request.result().map_err(|e| describe_js(&e))?;
    if value.is_undefined() || value.is_null() {
        return Ok(None);
    }
    key_pair_from(value).map(Some)
}

async fn add_slot(database: &IdbDatabase, pair: &CryptoKeyPair) -> Result<(), AddFailure> {
    let failure = |value: JsValue| AddFailure {
        error_name: "TransactionError".to_owned(),
        message: describe_js(&value),
    };
    let (transaction, store) =
        key_store(database, IdbTransactionMode::Readwrite).map_err(|message| AddFailure {
            error_name: "TransactionError".to_owned(),
            message,
        })?;
    let request = store
        .add_with_key(pair, &JsValue::from_str(DEVICE_KEY_SLOT))
        .map_err(failure)?;
    if transaction_settled(&transaction).await.is_ok() {
        return Ok(());
    }
    // The request's own error, not the transaction's: while the error event is
    // still bubbling the transaction has not been aborted yet, so its `error`
    // can be null exactly when the `add` hit an occupied slot.
    let exception = request
        .error()
        .ok()
        .flatten()
        .or_else(|| transaction.error());
    Err(match exception {
        Some(exception) => AddFailure {
            error_name: exception.name(),
            message: exception.message(),
        },
        None => AddFailure {
            error_name: "AbortError".to_owned(),
            message: "the device key transaction aborted".to_owned(),
        },
    })
}

async fn delete_slot(database: &IdbDatabase) -> Result<(), String> {
    let (transaction, store) = key_store(database, IdbTransactionMode::Readwrite)?;
    store
        .delete(&JsValue::from_str(DEVICE_KEY_SLOT))
        .map_err(|e| describe_js(&e))?;
    transaction_settled(&transaction)
        .await
        .map_err(|()| match transaction.error() {
            Some(exception) => describe_js(&exception),
            None => "the device key transaction aborted".to_owned(),
        })
}

fn key_store(
    database: &IdbDatabase,
    mode: IdbTransactionMode,
) -> Result<(IdbTransaction, IdbObjectStore), String> {
    let transaction = database
        .transaction_with_str_and_mode(KEY_STORE_NAME, mode)
        .map_err(|e| describe_js(&e))?;
    let store = transaction
        .object_store(KEY_STORE_NAME)
        .map_err(|e| describe_js(&e))?;
    Ok((transaction, store))
}

/// Resolve when a request succeeds, fail with its error when it does not.
async fn request_settled(request: &IdbRequest) -> Result<(), String> {
    let settled = Promise::new(&mut |resolve: Function, reject: Function| {
        request.set_onsuccess(Some(&resolve));
        request.set_onerror(Some(&reject));
    });
    let outcome = JsFuture::from(settled).await;
    request.set_onsuccess(None);
    request.set_onerror(None);
    match outcome {
        Ok(_) => Ok(()),
        Err(_) => Err(match request.error() {
            Ok(Some(exception)) => describe_js(&exception),
            Ok(None) | Err(_) => "the IndexedDB request failed".to_owned(),
        }),
    }
}

/// Resolve when a transaction commits; fail when it errors or aborts.
async fn transaction_settled(transaction: &IdbTransaction) -> Result<(), ()> {
    let settled = Promise::new(&mut |resolve: Function, reject: Function| {
        transaction.set_oncomplete(Some(&resolve));
        transaction.set_onerror(Some(&reject));
        transaction.set_onabort(Some(&reject));
    });
    let outcome = JsFuture::from(settled).await;
    transaction.set_oncomplete(None);
    transaction.set_onerror(None);
    transaction.set_onabort(None);
    outcome.map(|_| ()).map_err(|_| ())
}
