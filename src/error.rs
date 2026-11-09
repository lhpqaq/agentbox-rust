use std::fmt;
#[derive(Debug, Clone)]
pub struct AbError {
    pub code: u16,
    pub message: String,
    pub uncertain: bool,
    pub from_broker: bool,
}
pub type Result<T> = std::result::Result<T, AbError>;
impl AbError {
    pub fn new(code: u16, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            uncertain: false,
            from_broker: false,
        }
    }
    pub fn uncertain(mut self) -> Self {
        self.uncertain = true;
        self
    }
}
impl fmt::Display for AbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for AbError {}
impl From<std::io::Error> for AbError {
    fn from(e: std::io::Error) -> Self {
        Self::new(500, e.to_string())
    }
}
impl From<rusqlite::Error> for AbError {
    fn from(_: rusqlite::Error) -> Self {
        Self::new(500, "State journal unavailable; inspect private broker log")
    }
}
impl From<serde_json::Error> for AbError {
    fn from(_: serde_json::Error) -> Self {
        Self::new(422, "Invalid JSON")
    }
}
