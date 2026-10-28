//! tantivy 색인(계획서 8.1)과 검색. 결과는 메일 ID만 돌려주고 목록 행은 SQLite에서 채운다(8.3).

use std::collections::HashMap;
use std::ops::Bound;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use tantivy::collector::{Count, TopDocs};
use tantivy::directory::MmapDirectory;
use tantivy::query::{
    AllQuery, BooleanQuery, Occur, PhraseQuery, Query, RangeQuery, RegexQuery, TermQuery,
};
use tantivy::schema::{
    FAST, Field, INDEXED, IndexRecordOption, STORED, Schema, TextFieldIndexing, TextOptions,
};
use tantivy::tokenizer::TextAnalyzer;
use tantivy::{
    DocAddress, Index, IndexReader, IndexWriter, Order, ReloadPolicy, TantivyDocument, Term,
};

use crate::query::{Clause, FieldName, ParsedQuery, parse_query};
use crate::tokenizer::{INDEX_FORMAT, KoBigramTokenizer, TOKENIZER_NAME, tokenize_query};
use crate::{Error, Result};

/// 본문은 메일당 최대 200KB까지만 색인한다(계획서 8.1).
const MAX_BODY_BYTES: usize = 200 * 1024;
/// 색인 쓰기 버퍼와 스레드(1.0.3 메모리 점검: 전에는 `writer(128MB)`가 CPU 수만큼 최대 8스레드를 띄웠다).
/// 500통씩 묶어 확정하므로(sync 색인 큐) 스레드가 많아도 빠르지 않다. 5000통 측정: 최대 전용 메모리 135→54MB,
/// 시간은 같음(약 7초).
const WRITER_HEAP: usize = 64 * 1024 * 1024;
const WRITER_THREADS: usize = 2;

/// 색인할 메일 한 통.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MailDoc {
    pub id: u64,
    pub folder: u64,
    /// 받은 시각(unix ms).
    pub date: i64,
    pub subject: String,
    pub body: String,
    /// 이름 + 주소.
    pub from: String,
    pub to: String,
    pub cc: String,
    /// 내가 보낸 메일에만 있다.
    pub bcc: String,
    /// 첨부 파일 이름들.
    pub attach: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Sort {
    /// 최신순(fast field `date`, 점수 계산 생략).
    #[default]
    Newest,
    Relevance,
}

#[derive(Debug, Clone)]
pub struct SearchOptions {
    /// 현재 폴더만(없으면 전체).
    pub folder: Option<u64>,
    pub limit: usize,
    pub offset: usize,
    pub sort: Sort,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            folder: None,
            limit: 200,
            offset: 0,
            sort: Sort::Newest,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SearchResult {
    pub ids: Vec<u64>,
    /// 전체 일치 수.
    pub total: usize,
    pub elapsed: Duration,
}

struct Fields {
    id: Field,
    folder: Field,
    date: Field,
    text: HashMap<FieldName, Field>,
}

fn schema() -> Schema {
    let mut b = Schema::builder();
    b.add_u64_field("id", INDEXED | FAST | STORED);
    b.add_u64_field("folder", INDEXED | FAST);
    b.add_i64_field("date", INDEXED | FAST);
    let text = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer(TOKENIZER_NAME)
            .set_index_option(IndexRecordOption::WithFreqsAndPositions),
    );
    for f in FieldName::ALL {
        b.add_text_field(f.schema_name(), text.clone());
    }
    b.build()
}

pub struct SearchIndex {
    index: Index,
    reader: IndexReader,
    fields: Fields,
    writer: Mutex<Option<IndexWriter<TantivyDocument>>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn truncate_utf8(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

impl SearchIndex {
    /// `dir`의 색인을 열거나 새로 만든다(계획서 6장 `index\`).
    pub fn open(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir)?;
        let index = Index::open_or_create(MmapDirectory::open(dir)?, schema())?;
        Self::with_index(index)
    }

    /// 색인 형식([`INDEX_FORMAT`])을 확인하고 연다. 예전 형식이면 비우고 새로 만든다(그러면 true: 부른 쪽이
    /// 모든 메일을 다시 색인하게 표시한다). 형식 표시는 `dir\format`.
    pub fn open_checked(dir: &Path) -> Result<(Self, bool)> {
        let mark = dir.join("format");
        let current = std::fs::read_to_string(&mark)
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok());
        let stale = dir.exists() && current != Some(INDEX_FORMAT);
        if stale {
            for e in std::fs::read_dir(dir)?.flatten() {
                let p = e.path();
                if p.is_dir() {
                    std::fs::remove_dir_all(&p)?;
                } else {
                    std::fs::remove_file(&p)?;
                }
            }
        }
        let s = Self::open(dir)?;
        if current != Some(INDEX_FORMAT) {
            std::fs::write(&mark, INDEX_FORMAT.to_string())?;
        }
        Ok((s, stale))
    }

    /// 메모리 색인(테스트용).
    pub fn in_memory() -> Result<Self> {
        Self::with_index(Index::create_in_ram(schema()))
    }

    fn with_index(index: Index) -> Result<Self> {
        index
            .tokenizers()
            .register(TOKENIZER_NAME, TextAnalyzer::from(KoBigramTokenizer));
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()?;
        let s = index.schema();
        let field = |n: &str| s.get_field(n);
        let mut text = HashMap::new();
        for f in FieldName::ALL {
            text.insert(f, field(f.schema_name())?);
        }
        Ok(Self {
            fields: Fields {
                id: field("id")?,
                folder: field("folder")?,
                date: field("date")?,
                text,
            },
            index,
            reader,
            writer: Mutex::new(None),
        })
    }

    fn writer(&self) -> Result<MutexGuard<'_, Option<IndexWriter<TantivyDocument>>>> {
        let mut w = lock(&self.writer);
        if w.is_none() {
            *w = Some(
                self.index
                    .writer_with_num_threads(WRITER_THREADS, WRITER_HEAP)?,
            );
        }
        Ok(w)
    }

    /// 넣거나 바꾼다(같은 ID는 지우고 다시 넣음). 보이려면 [`SearchIndex::commit`].
    pub fn add(&self, docs: &[MailDoc]) -> Result<()> {
        let f = &self.fields;
        let mut guard = self.writer()?;
        let w = guard
            .as_mut()
            .ok_or_else(|| Error::Query("색인 쓰기 준비 실패".into()))?;
        for d in docs {
            w.delete_term(Term::from_field_u64(f.id, d.id));
            let mut doc = TantivyDocument::default();
            doc.add_u64(f.id, d.id);
            doc.add_u64(f.folder, d.folder);
            doc.add_i64(f.date, d.date);
            let texts = [
                (FieldName::Subject, d.subject.as_str()),
                (FieldName::Body, truncate_utf8(&d.body, MAX_BODY_BYTES)),
                (FieldName::From, d.from.as_str()),
                (FieldName::To, d.to.as_str()),
                (FieldName::Cc, d.cc.as_str()),
                (FieldName::Bcc, d.bcc.as_str()),
                (FieldName::Attach, d.attach.as_str()),
            ];
            for (name, value) in texts {
                if !value.is_empty() {
                    doc.add_text(f.text[&name], value);
                }
            }
            w.add_document(doc)?;
        }
        Ok(())
    }

    pub fn delete(&self, id: u64) -> Result<()> {
        let mut guard = self.writer()?;
        if let Some(w) = guard.as_mut() {
            w.delete_term(Term::from_field_u64(self.fields.id, id));
        }
        Ok(())
    }

    /// 쌓인 변경을 확정하고 바로 검색에 보이게 한다.
    pub fn commit(&self) -> Result<()> {
        if let Some(w) = lock(&self.writer).as_mut() {
            w.commit()?;
        }
        self.reader.reload()?;
        Ok(())
    }

    pub fn num_docs(&self) -> u64 {
        self.reader.searcher().num_docs()
    }

    /// 조항 하나를 한 필드에 대한 질의로. 토큰이 없으면 `None`.
    /// 영문·숫자 3글자 이상은 세 글자 조각 구문(단어 중간에 있어도 찾힘), 짧은 것과 한 글자 한글은 앞부분 일치.
    fn clause_query(&self, field: Field, c: &Clause) -> Option<Box<dyn Query>> {
        let tokens = tokenize_query(&c.text);
        let first = tokens.first()?;
        let term = |text: &str| Term::from_field_text(field, text);
        // 이메일 주소 단어는 통째 토큰 하나로 찾는다.
        if !c.phrase && first.text.contains('@') {
            return Some(Box::new(TermQuery::new(
                term(&first.text),
                IndexRecordOption::WithFreqs,
            )));
        }
        if tokens.len() == 1 {
            if first.prefix {
                // 토큰은 글자·숫자뿐이라 정규식 특수 문자가 없다.
                return RegexQuery::from_pattern(&format!("{}.*", first.text), field)
                    .ok()
                    .map(|q| Box::new(q) as Box<dyn Query>);
            }
            return Some(Box::new(TermQuery::new(
                term(&first.text),
                IndexRecordOption::WithFreqs,
            )));
        }
        Some(Box::new(PhraseQuery::new_with_offset(
            tokens.iter().map(|t| (t.position, term(&t.text))).collect(),
        )))
    }

    fn build_query(&self, parsed: &ParsedQuery, folder: Option<u64>) -> Box<dyn Query> {
        let mut must: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        for c in &parsed.clauses {
            let fields: Vec<FieldName> = match c.field {
                Some(f) => vec![f],
                None => FieldName::ALL.to_vec(),
            };
            let parts: Vec<Box<dyn Query>> = fields
                .iter()
                .filter_map(|f| self.clause_query(self.fields.text[f], c))
                .collect();
            match parts.len() {
                0 => {}
                1 => must.extend(parts.into_iter().map(|q| (Occur::Must, q))),
                _ => must.push((Occur::Must, Box::new(BooleanQuery::union(parts)))),
            }
        }
        if let Some(fid) = folder {
            must.push((
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_u64(self.fields.folder, fid),
                    IndexRecordOption::Basic,
                )),
            ));
        }
        if parsed.date_from.is_some() || parsed.date_to.is_some() {
            let bound = |v: Option<i64>, incl: bool| match v {
                None => Bound::Unbounded,
                Some(v) if incl => Bound::Included(Term::from_field_i64(self.fields.date, v)),
                Some(v) => Bound::Excluded(Term::from_field_i64(self.fields.date, v)),
            };
            must.push((
                Occur::Must,
                Box::new(RangeQuery::new(
                    bound(parsed.date_from, true),
                    bound(parsed.date_to, false),
                )),
            ));
        }
        if must.is_empty() {
            Box::new(AllQuery)
        } else {
            Box::new(BooleanQuery::new(must))
        }
    }

    /// 질의 문자열로 검색해 메일 ID를 돌려준다.
    pub fn search(&self, q: &str, opts: &SearchOptions) -> Result<SearchResult> {
        let started = Instant::now();
        let parsed = parse_query(q)?;
        let query = self.build_query(&parsed, opts.folder);
        let searcher = self.reader.searcher();
        let top = TopDocs::with_limit(opts.limit.max(1)).and_offset(opts.offset);
        let (addrs, total): (Vec<DocAddress>, usize) = match opts.sort {
            Sort::Newest => {
                let (docs, total) = searcher.search(
                    &query,
                    &(top.order_by_fast_field::<i64>("date", Order::Desc), Count),
                )?;
                (docs.into_iter().map(|(_, a)| a).collect(), total)
            }
            Sort::Relevance => {
                let (docs, total) = searcher.search(&query, &(top.order_by_score(), Count))?;
                (docs.into_iter().map(|(_, a)| a).collect(), total)
            }
        };
        let mut columns = HashMap::new();
        let mut ids = Vec::with_capacity(addrs.len());
        for a in addrs {
            let col = match columns.entry(a.segment_ord) {
                std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                std::collections::hash_map::Entry::Vacant(e) => e.insert(
                    searcher
                        .segment_reader(a.segment_ord)
                        .fast_fields()
                        .u64("id")?,
                ),
            };
            if let Some(id) = col.first(a.doc_id) {
                ids.push(id);
            }
        }
        Ok(SearchResult {
            ids,
            total,
            elapsed: started.elapsed(),
        })
    }

    /// 색인에 든 메일 ID 전부(DB와 대조해 고칠 때).
    pub fn all_ids(&self) -> Result<Vec<u64>> {
        let searcher = self.reader.searcher();
        let mut out = Vec::with_capacity(searcher.num_docs() as usize);
        for seg in searcher.segment_readers() {
            let col = seg.fast_fields().u64("id")?;
            for doc in seg.doc_ids_alive() {
                if let Some(id) = col.first(doc) {
                    out.push(id);
                }
            }
        }
        Ok(out)
    }

    /// 앱 시작 시 백그라운드로 한 번(계획서 8.4 워밍 질의).
    pub fn warm_up(&self) -> Result<()> {
        self.search("메일 보고서", &SearchOptions::default())
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(id: u64, folder: u64, date: i64, subject: &str, body: &str) -> MailDoc {
        MailDoc {
            id,
            folder,
            date,
            subject: subject.into(),
            body: body.into(),
            from: "김민수 <minsu.kim@example.com>".into(),
            to: "이지영 <jiyoung.lee@partner.co.kr>".into(),
            ..MailDoc::default()
        }
    }

    fn ids(ix: &SearchIndex, q: &str) -> Vec<u64> {
        let mut v = ix.search(q, &SearchOptions::default()).unwrap().ids;
        v.sort_unstable();
        v
    }

    /// 검색어로 시작하지 않아도 찾는다(0.7.0 사용자 요청): 영문·숫자 단어 중간, 한 글자 한글, 짧은 영문.
    #[test]
    fn finds_text_inside_words() {
        let ix = SearchIndex::in_memory().unwrap();
        ix.add(&[
            doc(1, 10, 1_000, "INC-12345 장애 보고", "인프라팀 확인 요청"),
            MailDoc {
                from: "Example Corp <noreply@example.com>".into(),
                ..doc(2, 10, 2_000, "Weekly report", "AI서비스 개선")
            },
        ])
        .unwrap();
        ix.commit().unwrap();
        assert_eq!(ids(&ix, "2345"), [1], "숫자 중간");
        assert_eq!(ids(&ix, "xampl"), [1, 2], "주소 조각 중간");
        assert_eq!(ids(&ix, "보낸이:reply"), [2], "주소 앞부분 중간");
        assert_eq!(ids(&ix, "eekl"), [2], "단어 중간");
        assert_eq!(ids(&ix, "ekly rep"), [2], "두 단어 모두 중간 일치(AND)");
        assert_eq!(ids(&ix, "\"weekly report\""), [2], "구문은 그대로");
        assert!(
            ids(&ix, "\"report weekly\"").is_empty(),
            "순서가 다르면 구문 아님"
        );
        assert_eq!(ids(&ix, "팀"), [1], "한 글자 한글이 낱말 끝에 있어도");
        assert_eq!(ids(&ix, "라팀"), [1]);
        assert_eq!(ids(&ix, "장"), [1], "한 글자 한글이 낱말 앞에 있어도");
        assert_eq!(ids(&ix, "ai"), [2], "짧은 영문은 앞부분 일치");
        assert!(ids(&ix, "kly2").is_empty(), "없는 글자열");
        assert_eq!(ids(&ix, "noreply@example.com"), [2], "주소 전체는 통째로");
    }

    #[test]
    fn korean_phrase_email_parts_fields_and_order() {
        let ix = SearchIndex::in_memory().unwrap();
        ix.add(&[
            doc(1, 10, 1_000, "월간보고서 제출", "이번 달 실적입니다"),
            doc(2, 10, 3_000, "회의 일정", "보고 드립니다. 서류는 내일"),
            MailDoc {
                bcc: "감사팀 <audit.bcc@secure-partner.co.kr>".into(),
                attach: "2026년_3분기_보고서.pdf".into(),
                ..doc(3, 20, 2_000, "Q3 결과", "첨부 확인 바랍니다")
            },
        ])
        .unwrap();
        ix.commit().unwrap();

        assert_eq!(
            ids(&ix, "보고서"),
            [1, 3],
            "제목·첨부 이름의 보고서, '보고 ... 서'는 아님"
        );
        let newest = ix.search("보고서", &SearchOptions::default()).unwrap();
        assert_eq!((newest.ids, newest.total), (vec![3, 1], 2), "최신순");
        assert_eq!(ids(&ix, "example"), [1, 2, 3]);
        assert_eq!(ids(&ix, "minsu.kim"), [1, 2, 3]);
        assert_eq!(ids(&ix, "jiyoung.lee@partner.co.kr"), [1, 2, 3]);
        assert_eq!(ids(&ix, "숨은참조:audit.bcc"), [3]);
        assert_eq!(ids(&ix, "받는이:audit.bcc"), Vec::<u64>::new());
        assert_eq!(ids(&ix, "보고서 실적"), [1], "AND");
        assert_eq!(ids(&ix, "제목:회의"), [2]);
        assert_eq!(ids(&ix, "q3"), [3]);
        assert_eq!(
            ix.search(
                "보고서",
                &SearchOptions {
                    folder: Some(20),
                    ..SearchOptions::default()
                }
            )
            .unwrap()
            .ids,
            [3]
        );

        // 바꿔 넣으면 이전 내용은 사라진다.
        ix.add(&[doc(1, 30, 1_000, "변경됨", "")]).unwrap();
        ix.commit().unwrap();
        assert_eq!(ids(&ix, "보고서"), [3]);
        assert_eq!(ix.num_docs(), 3);
    }
}
