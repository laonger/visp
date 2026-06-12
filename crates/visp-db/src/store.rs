use std::sync::Mutex;

/// SQLite-backed session store implementing `SessionStore` trait.
#[allow(dead_code)]
pub struct SqliteSessionStore {
    conn: Mutex<rusqlite::Connection>,
}
