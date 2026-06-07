use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct ReportResponse {
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
}
