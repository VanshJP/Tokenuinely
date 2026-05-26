/// Database connection and query layer for the sample application.
/// Manages SQLite connections and provides CRUD operations.

use std::path::Path;

pub struct Database {
    path: String,
}

impl Database {
    pub fn connect(path: &Path) -> Result<Self, DatabaseError> {
        if !path.exists() {
            return Err(DatabaseError::ConnectionFailed(
                "Database file not found".into(),
            ));
        }
        Ok(Self {
            path: path.to_string_lossy().to_string(),
        })
    }

    pub fn query(&self, sql: &str) -> Result<Vec<String>, DatabaseError> {
        if sql.is_empty() {
            return Err(DatabaseError::QueryError("Empty query".into()));
        }
        // Placeholder: return the path and query for testing
        Ok(vec![format!("{}:{}", self.path, sql)])
    }

    pub fn execute(&self, sql: &str) -> Result<usize, DatabaseError> {
        if sql.is_empty() {
            return Err(DatabaseError::QueryError("Empty statement".into()));
        }
        Ok(1) // affected rows
    }
}

#[derive(Debug)]
pub enum DatabaseError {
    ConnectionFailed(String),
    QueryError(String),
}
