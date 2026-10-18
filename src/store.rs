//! Repository-neutral application boundary. SQLite exists only for local reference tests.
use crate::{
    core::{AppError, Core, Principal},
    firestore_core::FirestoreCore,
};
use serde_json::Value;
use std::collections::BTreeMap;
pub enum Store {
    Local(Core),
    Firestore(FirestoreCore),
}
macro_rules! dispatch {($name:ident($($arg:ident:$ty:ty),* $(,)?)->$ret:ty)=>{#[allow(clippy::too_many_arguments)] pub async fn $name(&self,$($arg:$ty),*)->Result<$ret,AppError>{match self{Self::Local(s)=>s.$name($($arg),*),Self::Firestore(s)=>s.$name($($arg),*).await}}};}
impl Store {
    dispatch!(list_shares(p: &Principal) -> Value);
    dispatch!(
        create_share(
            p: &Principal,
            title: &str,
            kind: &str,
            policy: &str,
            idempotency_key: &str,
        ) -> Value
    );
    dispatch!(get_share(p: &Principal, id: &str) -> Value);
    dispatch!(create_upload(p: &Principal, id: &str) -> Value);
    dispatch!(get_upload(p: &Principal, id: &str) -> Value);
    dispatch!(
        stage_upload(
            p: &Principal,
            id: &str,
            files: &BTreeMap<String, Vec<u8>>,
            entrypoint: &str,
        ) -> Value
    );
    dispatch!(staged_files(p: &Principal, id: &str) -> (BTreeMap<String, Vec<u8>>, String));
    dispatch!(begin_finalize(p: &Principal, id: &str, key: &str, entrypoint: &str) -> Value);
    dispatch!(
        publish_finalized(
            p: &Principal,
            id: &str,
            key: &str,
            files: &BTreeMap<String, Vec<u8>>,
            entrypoint: &str,
            warnings: &Value,
        ) -> Value
    );
    dispatch!(
        publish(
            p: &Principal,
            id: &str,
            files: &BTreeMap<String, Vec<u8>>,
            entrypoint: &str,
        ) -> Value
    );
    dispatch!(viewer_share(cap: &str, p: Option<&Principal>) -> Value);
    dispatch!(viewer_revision(cap: &str, revision: Option<&str>, p: Option<&Principal>) -> Value);
    dispatch!(
        revision_files(
            cap: &str,
            revision: Option<&str>,
            p: Option<&Principal>,
        ) -> BTreeMap<String, Vec<u8>>
    );
    dispatch!(threads(cap: &str, p: Option<&Principal>) -> Value);
    dispatch!(
        add_thread(
            cap: &str,
            p: Option<&Principal>,
            name: Option<&str>,
            body: &str,
            revision: &str,
            anchor: Value,
        ) -> Value
    );
    dispatch!(
        reply(cap: &str, id: &str, p: Option<&Principal>, name: Option<&str>, body: &str) -> Value
    );
    dispatch!(
        add_thread_idempotent(
            cap: &str,
            p: Option<&Principal>,
            name: Option<&str>,
            body: &str,
            revision: &str,
            anchor: Value,
            key: &str,
            actor_scope: &str,
        ) -> Value
    );
    dispatch!(
        reply_idempotent(
            cap: &str,
            id: &str,
            p: Option<&Principal>,
            name: Option<&str>,
            body: &str,
            key: &str,
            actor_scope: &str,
        ) -> Value
    );
    dispatch!(resolve(p: &Principal, id: &str, thread: &str, resolved: bool) -> Value);
    dispatch!(lifecycle(p: &Principal, id: &str, action: &str) -> Value);
    dispatch!(notification_status(p: &Principal, id: &str) -> Value);
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Local(_) => "local-reference",
            Self::Firestore(_) => "firestore",
        }
    }
}
