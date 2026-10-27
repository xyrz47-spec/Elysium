use serde::{Serialize, Serializer};

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("데이터베이스 오류: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("JSON 오류: {0}")]
    Json(#[from] serde_json::Error),
    #[error("입출력 오류: {0}")]
    Io(#[from] std::io::Error),
    #[error("정책 거부: {0}")]
    Policy(String),
    #[error("찾을 수 없음: {0}")]
    NotFound(String),
    #[error("{0}")]
    Msg(String),
}

impl AppError {
    pub fn msg(s: impl Into<String>) -> Self {
        AppError::Msg(s.into())
    }
}

// 프런트엔드에는 사람이 읽을 수 있는 문자열로만 전달한다.
impl Serialize for AppError {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

pub type AppResult<T> = Result<T, AppError>;
