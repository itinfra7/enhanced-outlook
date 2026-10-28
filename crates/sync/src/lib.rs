//! 동기화 엔진: 계층 동기화, 폴더별 항목 동기화, 서버 삭제 판정(이동 짝짓기), 원문 큐.
//!
//! 계획서 6.3, 7장. 서버와 이야기하는 유일한 계층이다. 작업 큐(ops) 처리는 5단계에서 붙인다
//! (큐 저장·합치기·백오프는 `eo-store`에 있다).

mod apply;
mod convert;
mod domino;
mod duplicates;
mod engine;
mod import;
mod index;
mod ops;
mod ost;
mod pst;
mod send;

pub use apply::{ApplyStats, ArchiveRecord, FolderChanges, MoveRecord, apply_cycle};
pub use domino::{
    DominoOpsReport, DominoOptions, DominoReport, DominoScan, ListedMail, push_domino_ops,
    sync_domino,
};
pub use duplicates::{BodyCache, DupScan, DupSet, exact_duplicates};
pub use engine::{
    CycleReport, EmlReport, Engine, EngineConfig, Event, HierarchyReport, ItemsReport,
};
pub use import::{
    DOMINO_FOLDER, ImportOptions, ImportProgress, ImportReport, import_paths, import_paths_with,
    repair_domino_eml, write_sample_domino_backup, write_sample_html_backup,
};
pub use index::{IndexReport, index_pending, reconcile_index};
pub use ops::OpsReport;
pub use ost::OstFile;
pub use pst::PstItem;
pub use send::QUOTE_CID_BASE;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Owa(#[from] eo_owa::Error),
    #[error(transparent)]
    Store(#[from] eo_store::Error),
    #[error(transparent)]
    Search(#[from] eo_search::Error),
    #[error("작업 실행 실패: {0}")]
    Task(String),
}
