pub mod bootstrap;
pub mod channels;
pub mod keys;
pub mod models;
pub mod schema;
pub mod tokens;
pub mod usage;
pub mod users;

use chennix_common::ProxyResult;
use rusqlite::Connection;

pub fn open_db(path: &str) -> ProxyResult<Connection> {
    let conn = Connection::open(path)
        .map_err(|e| chennix_common::ProxyError::Storage(e.to_string()))?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
        .map_err(|e| chennix_common::ProxyError::Storage(e.to_string()))?;
    schema::init_db(&conn)?;
    Ok(conn)
}
