//! IndexedDB-backed [`Storage`] for the web frontend.
//!
//! # The sync/async impedance mismatch
//!
//! `rustyboi_session::Storage` is a *synchronous* trait (`read`/`write`/`list`
//! return immediately). IndexedDB is *asynchronous* — every operation is a
//! request that resolves via an event. We bridge the two with a write-through
//! in-memory cache:
//!
//! - A `HashMap<String, Vec<u8>>` is the actual sync `Storage` the session
//!   drives. Reads and lists hit it directly; they never touch the DB.
//! - `write` updates the map synchronously *and* fires an async IndexedDB
//!   `put` (fire-and-forget) to persist the blob across reloads.
//! - At startup, [`IdbStore::hydrate`] loads every key from IndexedDB into the
//!   map *before* the session is built, so the first synchronous `read` already
//!   sees persisted config / saves / SRAM.
//!
//! This keeps the session WASM-clean (no async in the hot path) while giving
//! real cross-reload persistence in Firefox — where the File System Access API
//! is unavailable, IndexedDB is the correct durable store.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use js_sys::Uint8Array;
use rustyboi_session::{Storage, StorageError};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{IdbDatabase, IdbFactory, IdbOpenDbRequest, IdbRequest, IdbTransactionMode};

const DB_NAME: &str = "rustyboi";
const STORE_NAME: &str = "kv";
const DB_VERSION: u32 = 1;

/// The synchronous cache the session sees, shared with the async DB mirror.
type Cache = Rc<RefCell<HashMap<String, Vec<u8>>>>;

/// A [`Storage`] adapter: synchronous in-memory cache, asynchronously mirrored
/// to IndexedDB. Cheap to clone (shares the cache + DB handle via `Rc`).
#[derive(Clone)]
pub struct IdbStore {
    cache: Cache,
    db: Rc<IdbDatabase>,
}

impl IdbStore {
    /// Open (or create) the IndexedDB database, then load every stored key into
    /// the in-memory cache. Async because opening a DB and reading it out are
    /// both event-driven; call and `.await` this once at startup before the
    /// session exists.
    pub async fn open_and_hydrate() -> Result<IdbStore, JsValue> {
        let db = open_db().await?;
        let cache: Cache = Rc::new(RefCell::new(HashMap::new()));
        hydrate(&db, &cache).await?;
        Ok(IdbStore { cache, db: Rc::new(db) })
    }

    /// Number of cached keys (diagnostic).
    pub fn len(&self) -> usize {
        self.cache.borrow().len()
    }

    /// Fire-and-forget async `put` of one key into IndexedDB.
    fn persist(&self, key: String, data: Vec<u8>) {
        let db = self.db.clone();
        wasm_bindgen_futures::spawn_local(async move {
            if let Err(e) = put(&db, &key, &data).await {
                web_sys::console::warn_1(&format!("idb put {key} failed: {e:?}").into());
            }
        });
    }
}

impl Storage for IdbStore {
    fn read(&self, key: &str) -> Option<Vec<u8>> {
        self.cache.borrow().get(key).cloned()
    }

    fn write(&mut self, key: &str, data: &[u8]) -> Result<(), StorageError> {
        self.cache.borrow_mut().insert(key.to_string(), data.to_vec());
        self.persist(key.to_string(), data.to_vec());
        Ok(())
    }

    fn list(&self, prefix: &str) -> Vec<String> {
        self.cache
            .borrow()
            .keys()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect()
    }
}

// --- raw IndexedDB plumbing -------------------------------------------------

/// Await an `IdbRequest`, resolving to its `result` on success.
async fn await_request(req: &IdbRequest) -> Result<JsValue, JsValue> {
    let promise = js_sys::Promise::new(&mut |resolve, reject| {
        let req_ok = req.clone();
        let onsuccess = Closure::once(Box::new(move |_e: web_sys::Event| {
            let _ = resolve.call1(&JsValue::NULL, &req_ok.result().unwrap_or(JsValue::NULL));
        }) as Box<dyn FnOnce(_)>);
        let onerror = Closure::once(Box::new(move |_e: web_sys::Event| {
            let _ = reject.call1(&JsValue::NULL, &JsValue::from_str("idb request error"));
        }) as Box<dyn FnOnce(_)>);
        req.set_onsuccess(Some(onsuccess.as_ref().unchecked_ref()));
        req.set_onerror(Some(onerror.as_ref().unchecked_ref()));
        onsuccess.forget();
        onerror.forget();
    });
    JsFuture::from(promise).await
}

/// IndexedDB factory from whichever global scope we're in. The emulator runs in
/// a Web Worker (which has no `window`), so we read `indexedDB` off the global
/// object directly — it exists on both `Window` and `WorkerGlobalScope`.
fn idb_factory() -> Result<IdbFactory, JsValue> {
    let global = js_sys::global();
    let idb = js_sys::Reflect::get(&global, &JsValue::from_str("indexedDB"))?;
    if idb.is_undefined() || idb.is_null() {
        return Err(JsValue::from_str("indexedDB unavailable"));
    }
    idb.dyn_into::<IdbFactory>()
}

async fn open_db() -> Result<IdbDatabase, JsValue> {
    let factory = idb_factory()?;
    let open_req: IdbOpenDbRequest = factory.open_with_u32(DB_NAME, DB_VERSION)?;

    // Create the object store on first open / version bump.
    let upgrade = Closure::once(Box::new(move |e: web_sys::Event| {
        let target = e.target().unwrap();
        let req: IdbOpenDbRequest = target.dyn_into().unwrap();
        let db: IdbDatabase = req.result().unwrap().dyn_into().unwrap();
        if !db.object_store_names().contains(STORE_NAME) {
            let _ = db.create_object_store(STORE_NAME);
        }
    }) as Box<dyn FnOnce(_)>);
    open_req.set_onupgradeneeded(Some(upgrade.as_ref().unchecked_ref()));
    upgrade.forget();

    let result = await_request(open_req.as_ref()).await?;
    result.dyn_into::<IdbDatabase>()
}

/// Load every (key, value) pair from the object store into `cache`.
async fn hydrate(db: &IdbDatabase, cache: &Cache) -> Result<(), JsValue> {
    let tx = db.transaction_with_str(STORE_NAME)?;
    let store = tx.object_store(STORE_NAME)?;

    // Keys and values in matching order.
    let keys = await_request(&store.get_all_keys()?).await?;
    let vals = await_request(&store.get_all()?).await?;
    let keys = js_sys::Array::from(&keys);
    let vals = js_sys::Array::from(&vals);

    let mut map = cache.borrow_mut();
    for i in 0..keys.length() {
        if let Some(k) = keys.get(i).as_string() {
            let v = vals.get(i);
            if let Ok(arr) = v.dyn_into::<Uint8Array>() {
                map.insert(k, arr.to_vec());
            }
        }
    }
    Ok(())
}

async fn put(db: &IdbDatabase, key: &str, data: &[u8]) -> Result<(), JsValue> {
    let tx = db.transaction_with_str_and_mode(STORE_NAME, IdbTransactionMode::Readwrite)?;
    let store = tx.object_store(STORE_NAME)?;
    let arr = Uint8Array::from(data);
    let req = store.put_with_key(arr.as_ref(), &JsValue::from_str(key))?;
    await_request(&req).await?;
    Ok(())
}
