//! Erreur « HTTP » renvoyée au JavaScript : statut + message, comme l'API de RustITM.

use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct AppError {
    pub status: u16,
    #[serde(rename = "error")]
    pub message: String,
    pub itm_code: Option<i32>,
}

impl AppError {
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self {
            status: 400,
            message: msg.into(),
            itm_code: None,
        }
    }
    pub fn internal(msg: impl Into<String>) -> Self {
        Self {
            status: 500,
            message: msg.into(),
            itm_code: None,
        }
    }
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| r#"{"status":500,"error":"?"}"#.into())
    }
}

impl From<itm::ItmError> for AppError {
    fn from(e: itm::ItmError) -> Self {
        Self {
            status: 422,
            message: e.message().into(),
            itm_code: Some(e.code()),
        }
    }
}

impl From<serde_json::Error> for AppError {
    fn from(e: serde_json::Error) -> Self {
        Self::bad_request(format!("invalid JSON request: {e}"))
    }
}
