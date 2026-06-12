use std::sync::Mutex;
use visp_core::session::SessionStore;

/// SQLite-backed session store implementing `SessionStore` trait.
pub struct SqliteSessionStore {
    conn: Mutex<rusqlite::Connection>,
}
