//! 로컬 검색: tantivy 색인, `ko_bigram` 토크나이저, 질의 해석.
//!
//! 계획서 8장. 서버 측 검색은 쓰지 않는다.

mod index;
mod query;
pub mod synth;
mod tokenizer;

pub use index::{MailDoc, SearchIndex, SearchOptions, SearchResult, Sort};
pub use query::{Clause, FieldName, ParsedQuery, parse_query};
pub use tokenizer::{
    INDEX_FORMAT, KoBigramTokenizer, QueryToken, TOKENIZER_NAME, tokenize, tokenize_query,
};

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("색인 오류: {0}")]
    Tantivy(#[from] tantivy::TantivyError),
    #[error("색인 폴더 오류: {0}")]
    Directory(#[from] tantivy::directory::error::OpenDirectoryError),
    #[error("파일 오류: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Query(String),
}
