//! Outlook 2013 이후 OST 형식(4K 페이지, `wVer` 36) → EML. HCL Traveler for Microsoft Outlook이 메일함을 백업한
//! `.ims` 파일이 이 형식이다(사용자 요청 2026-10-23, 샘플로 확인). Microsoft `outlook-pst`는 512바이트 페이지
//! PST(`wVer` 14·15·23)만 읽어서, 메일을 꺼내는 데 필요한 만큼(노드·블록 B-트리, 블록, 데이터·하위 노드 트리, 힙·
//! BTH·속성 문맥·표 문맥)을 여기서 읽는다. 읽기만 한다. 메일은 PST와 같은 길([`crate::pst::build`])로 다시 만든다.
//!
//! 512바이트 페이지 PST와 다른 점(샘플로 확인):
//! - B-트리 페이지 4096바이트: 항목 4056바이트, 개수·최대 개수 u16, 항목 크기·단계 u8, 빈 칸 10바이트, 꼬리 24바이트
//!   (종류·종류·서명·CRC·페이지 ID·8바이트).
//! - 블록 B-트리 맨 아래 항목 24바이트: BREF 16, 저장 크기 u16, 풀린 크기 u16, 참조 수 u16, 빈 칸 2.
//! - 블록은 512바이트 단위로 놓이고 끝 24바이트가 꼬리다(저장 크기·서명·CRC·블록 ID·?·풀린 크기·빈 칸).
//!   풀린 크기가 저장 크기보다 크면 zlib으로 압축돼 있다.
//!
//! 나머지(헤더의 루트 위치, 노드 B-트리 항목 32바이트, XBLOCK·SLBLOCK·SIBLOCK, 힙 이하)는 Unicode PST와 같다.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::pst::{
    PR_CONTAINER_CLASS, PR_DISPLAY_NAME, PR_MESSAGE_CLASS, Parts, Props, PstItem, Val, build,
    filetime_ms, is_mail_folder, string8, text,
};

const PAGE: usize = 4096;
/// 페이지 안 항목 영역.
const PAGE_ENTRIES: usize = 4056;
const BLOCK_ALIGN: u64 = 512;
const BLOCK_TRAILER: u64 = 24;
const PTYPE_BBT: u8 = 0x80;
const PTYPE_NBT: u8 = 0x81;
/// B-트리 페이지를 이보다 많이 읽으면 고장 난 파일로 본다.
const MAX_PAGES: usize = 1 << 20;

const NID_MESSAGE_STORE: u32 = 0x21;
const NID_ROOT_FOLDER: u32 = 0x122;
const NID_ATTACHMENT_TABLE: u32 = 0x671;
const NID_RECIPIENT_TABLE: u32 = 0x692;
const NID_TYPE_HIERARCHY_TABLE: u32 = 0x0D;
const NID_TYPE_CONTENTS_TABLE: u32 = 0x0E;

const PR_IPM_SUBTREE_ENTRYID: u16 = 0x35E0;
const PR_ATTACH_METHOD: u16 = 0x3705;
const PR_ATTACH_DATA: u16 = 0x3701;
/// 첨부 방식: 첨부된 메일.
const ATTACH_EMBEDDED_MSG: i32 = 5;

const PT_I2: u16 = 0x0002;
const PT_LONG: u16 = 0x0003;
const PT_FLOAT: u16 = 0x0004;
const PT_ERROR: u16 = 0x000A;
const PT_BOOLEAN: u16 = 0x000B;
const PT_OBJECT: u16 = 0x000D;
const PT_STRING8: u16 = 0x001E;
const PT_UNICODE: u16 = 0x001F;
const PT_SYSTIME: u16 = 0x0040;
const PT_CLSID: u16 = 0x0048;
const PT_BINARY: u16 = 0x0102;

fn u16le(b: &[u8], at: usize) -> u16 {
    b.get(at..at + 2)
        .map_or(0, |x| u16::from_le_bytes([x[0], x[1]]))
}

fn u32le(b: &[u8], at: usize) -> u32 {
    b.get(at..at + 4)
        .map_or(0, |x| u32::from_le_bytes([x[0], x[1], x[2], x[3]]))
}

fn u64le(b: &[u8], at: usize) -> u64 {
    b.get(at..at + 8).map_or(0, |x| {
        u64::from_le_bytes([x[0], x[1], x[2], x[3], x[4], x[5], x[6], x[7]])
    })
}

/// 노드: 데이터 블록과 하위 노드 블록.
#[derive(Debug, Clone, Copy)]
struct Node {
    data: u64,
    sub: u64,
}

#[derive(Debug, Clone, Copy)]
struct BlockEntry {
    ib: u64,
    cb: u16,
    inflated: u16,
}

/// 열어 둔 OST(4K) 하나.
pub struct OstFile {
    file: RefCell<File>,
    nodes: HashMap<u32, Node>,
    blocks: HashMap<u64, BlockEntry>,
}

/// 하위 노드 표(노드 번호 → 노드).
type SubNodes = HashMap<u32, Node>;

/// 속성 문맥: 속성 번호 → (형식, 값 바이트).
type RawProps = HashMap<u16, (u16, Vec<u8>)>;

/// BTH 한 줄: (열쇠, 값).
type Record = (Vec<u8>, Vec<u8>);

impl OstFile {
    pub fn open(path: &Path) -> Result<Self, String> {
        let mut f = File::open(path).map_err(|e| e.to_string())?;
        let mut h = [0u8; 564];
        f.read_exact(&mut h)
            .map_err(|_| "Outlook 데이터 파일이 아닙니다".to_owned())?;
        if &h[0..4] != b"!BDN" {
            return Err("Outlook 데이터 파일이 아닙니다".into());
        }
        let ver = u16le(&h, 10);
        if ver != 36 {
            return Err(format!(
                "Outlook 2013 이후 OST 형식(4K)이 아닙니다(버전 {ver})"
            ));
        }
        if h[513] != 0 {
            return Err(format!(
                "암호화된 데이터 파일은 아직 읽지 못합니다(방식 {})",
                h[513]
            ));
        }
        let mut ost = Self {
            file: RefCell::new(f),
            nodes: HashMap::new(),
            blocks: HashMap::new(),
        };
        let nbt = u64le(&h, 224);
        let bbt = u64le(&h, 240);
        ost.walk(bbt, PTYPE_BBT)?;
        ost.walk(nbt, PTYPE_NBT)?;
        Ok(ost)
    }

    fn read_at(&self, ib: u64, len: usize) -> Result<Vec<u8>, String> {
        let mut f = self.file.borrow_mut();
        f.seek(SeekFrom::Start(ib)).map_err(|e| e.to_string())?;
        let mut buf = vec![0; len];
        f.read_exact(&mut buf)
            .map_err(|e| format!("{ib:#x} 읽기 실패: {e}"))?;
        Ok(buf)
    }

    /// B-트리를 끝까지 읽어 노드·블록 표를 채운다.
    fn walk(&mut self, root: u64, ptype: u8) -> Result<(), String> {
        let mut stack = vec![root];
        let mut pages = 0;
        while let Some(ib) = stack.pop() {
            pages += 1;
            if pages > MAX_PAGES {
                return Err("B-트리가 너무 깁니다(고장 난 파일)".into());
            }
            let p = self.read_at(ib, PAGE)?;
            if p[4072] != ptype || p[4073] != ptype {
                return Err(format!("{ib:#x}: B-트리 페이지가 아닙니다"));
            }
            let count = usize::from(u16le(&p, 4056));
            let size = usize::from(p[4060]);
            let level = p[4061];
            if size == 0 || count * size > PAGE_ENTRIES {
                return Err(format!("{ib:#x}: B-트리 페이지가 고장 났습니다"));
            }
            for e in p[..count * size].chunks_exact(size) {
                if level > 0 {
                    // BTENTRY: 열쇠 8, BREF(블록 ID 8, 위치 8)
                    stack.push(u64le(e, 16));
                } else if ptype == PTYPE_BBT {
                    self.blocks.insert(
                        u64le(e, 0) & !1,
                        BlockEntry {
                            ib: u64le(e, 8),
                            cb: u16le(e, 16),
                            inflated: u16le(e, 18),
                        },
                    );
                } else {
                    // NBTENTRY: 노드 8, 데이터 블록 8, 하위 노드 블록 8, 부모 4, 빈 칸 4
                    self.nodes.insert(
                        u64le(e, 0) as u32,
                        Node {
                            data: u64le(e, 8),
                            sub: u64le(e, 16),
                        },
                    );
                }
            }
        }
        Ok(())
    }

    /// 블록 하나(압축이면 풀어서).
    fn block(&self, bid: u64) -> Result<Vec<u8>, String> {
        let e = *self
            .blocks
            .get(&(bid & !1))
            .ok_or_else(|| format!("블록 {bid:#x} 없음"))?;
        let total = (u64::from(e.cb) + BLOCK_TRAILER).div_ceil(BLOCK_ALIGN) * BLOCK_ALIGN;
        let raw = self.read_at(e.ib, usize::try_from(total).unwrap_or(0))?;
        let trailer = &raw[raw.len() - BLOCK_TRAILER as usize..];
        if u64le(trailer, 8) & !1 != bid & !1 {
            return Err(format!("블록 {bid:#x}: 꼬리의 블록 ID가 다릅니다"));
        }
        let data = &raw[..usize::from(e.cb)];
        // 압축하지 않은 블록은 두 크기가 같다.
        if e.inflated != 0 && e.inflated != e.cb {
            let mut out = Vec::with_capacity(usize::from(e.inflated));
            flate2::read::ZlibDecoder::new(data)
                .read_to_end(&mut out)
                .map_err(|err| format!("블록 {bid:#x} 압축 풀기 실패: {err}"))?;
            return Ok(out);
        }
        Ok(data.to_vec())
    }

    /// 데이터 트리의 블록들(XBLOCK·XXBLOCK이면 풀어서 차례대로).
    fn data(&self, bid: u64) -> Result<Vec<Vec<u8>>, String> {
        if bid == 0 {
            return Ok(Vec::new());
        }
        let b = self.block(bid)?;
        if bid & 2 == 0 {
            return Ok(vec![b]);
        }
        if b.first() != Some(&1) || b[1] > 2 {
            return Err(format!("블록 {bid:#x}: 데이터 트리가 아닙니다"));
        }
        let count = usize::from(u16le(&b, 2));
        let mut out = Vec::new();
        for i in 0..count {
            let child = u64le(&b, 8 + i * 8);
            if b[1] == 1 {
                out.push(self.block(child)?);
            } else {
                out.extend(self.data(child)?);
            }
        }
        Ok(out)
    }

    /// 하위 노드 트리(SLBLOCK·SIBLOCK).
    fn subnodes(&self, bid: u64) -> Result<SubNodes, String> {
        let mut out = SubNodes::new();
        if bid == 0 {
            return Ok(out);
        }
        let mut stack = vec![(bid, 0)];
        while let Some((bid, depth)) = stack.pop() {
            if depth > 4 {
                return Err("하위 노드 트리가 너무 깊습니다".into());
            }
            let b = self.block(bid)?;
            if b.first() != Some(&2) {
                return Err(format!("블록 {bid:#x}: 하위 노드 트리가 아닙니다"));
            }
            let count = usize::from(u16le(&b, 2));
            for i in 0..count {
                if b[1] == 0 {
                    let at = 8 + i * 24;
                    out.insert(
                        u64le(&b, at) as u32,
                        Node {
                            data: u64le(&b, at + 8),
                            sub: u64le(&b, at + 16),
                        },
                    );
                } else {
                    stack.push((u64le(&b, 8 + i * 16 + 8), depth + 1));
                }
            }
        }
        Ok(out)
    }

    fn node(&self, nid: u32) -> Result<Node, String> {
        self.nodes
            .get(&nid)
            .copied()
            .ok_or_else(|| format!("노드 {nid:#x} 없음"))
    }

    /// 힙 항목이나 하위 노드의 값(HNID).
    fn hnid(&self, heap: &Heap, subs: &SubNodes, hnid: u32) -> Result<Vec<u8>, String> {
        if hnid == 0 {
            return Ok(Vec::new());
        }
        if hnid & 0x1F == 0 {
            return Ok(heap.item(hnid).map(<[u8]>::to_vec).unwrap_or_default());
        }
        match subs.get(&hnid) {
            Some(n) => Ok(self.data(n.data)?.concat()),
            None => Ok(Vec::new()),
        }
    }

    /// 속성 문맥(노드 데이터) + 그 노드의 하위 노드.
    fn pc(&self, node: Node) -> Result<(RawProps, SubNodes), String> {
        let heap = Heap::read(self, node.data)?;
        if heap.client() != 0xBC {
            return Err("속성 문맥이 아닙니다".into());
        }
        let subs = self.subnodes(node.sub)?;
        let mut out = RawProps::new();
        for (key, data) in heap.bth(heap.root())? {
            let id = u16le(&key, 0);
            let ptype = u16le(&data, 0);
            let v = u32le(&data, 2);
            let bytes = match ptype {
                PT_I2 | PT_LONG | PT_FLOAT | PT_ERROR | PT_BOOLEAN => v.to_le_bytes().to_vec(),
                _ => self.hnid(&heap, &subs, v)?,
            };
            out.insert(id, (ptype, bytes));
        }
        Ok((out, subs))
    }

    /// 표 문맥의 행들(행 번호, 속성).
    fn table(&self, node: Node) -> Result<Vec<(u32, RawProps)>, String> {
        let heap = Heap::read(self, node.data)?;
        if heap.client() != 0x7C {
            return Err("표 문맥이 아닙니다".into());
        }
        let info = heap.item(heap.root()).ok_or("표 정보가 없습니다")?.to_vec();
        if info.first() != Some(&0x7C) {
            return Err("표 정보가 고장 났습니다".into());
        }
        let cols: Vec<(u32, usize, usize, usize)> = (0..usize::from(info[1]))
            .map(|i| {
                let at = 22 + i * 8;
                (
                    u32le(&info, at),
                    usize::from(u16le(&info, at + 4)),
                    usize::from(info.get(at + 6).copied().unwrap_or(0)),
                    usize::from(info.get(at + 7).copied().unwrap_or(0)),
                )
            })
            .collect();
        let ceb = usize::from(u16le(&info, 6));
        let row_size = usize::from(u16le(&info, 8));
        let rows_hnid = u32le(&info, 14);
        if row_size == 0 || rows_hnid == 0 {
            return Ok(Vec::new());
        }
        let subs = self.subnodes(node.sub)?;
        let blocks = if rows_hnid & 0x1F == 0 {
            vec![heap.item(rows_hnid).map(<[u8]>::to_vec).unwrap_or_default()]
        } else {
            match subs.get(&rows_hnid) {
                Some(n) => self.data(n.data)?,
                None => Vec::new(),
            }
        };
        let mut out = Vec::new();
        for block in &blocks {
            for row in block.chunks_exact(row_size) {
                let id = u32le(row, 0);
                let mut props = RawProps::new();
                for &(tag, ib, cb, bit) in &cols {
                    let exists = row
                        .get(ceb + bit / 8)
                        .is_some_and(|b| b & (0x80 >> (bit % 8)) != 0);
                    if !exists {
                        continue;
                    }
                    let ptype = (tag & 0xFFFF) as u16;
                    let Some(cell) = row.get(ib..ib + cb) else {
                        continue;
                    };
                    let bytes = if is_variable(ptype) {
                        self.hnid(&heap, &subs, u32le(cell, 0))?
                    } else {
                        cell.to_vec()
                    };
                    props.insert((tag >> 16) as u16, (ptype, bytes));
                }
                out.push((id, props));
            }
        }
        Ok(out)
    }

    /// 하위 폴더들(노드 번호, 속성).
    fn children(&self, folder: u32) -> Result<Vec<(u32, Props)>, String> {
        let Ok(n) = self.node((folder & !0x1F) | NID_TYPE_HIERARCHY_TABLE) else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for (child, _) in self.table(n)? {
            let Ok(cn) = self.node(child) else { continue };
            let Ok((fp, _)) = self.pc(cn) else { continue };
            out.push((child, props(&fp)));
        }
        Ok(out)
    }

    /// IPM 하위 폴더(받은 편지함 등이 있는 곳). 저장소 속성에 없으면(샘플 `.ims`가 그랬다) 루트 아래에서
    /// 이름이 `IPM_SUBTREE`인 폴더를 찾는다.
    fn ipm_subtree(&self) -> Result<u32, String> {
        let (store, _) = self.pc(self.node(NID_MESSAGE_STORE)?)?;
        if let Some(n) = store
            .get(&PR_IPM_SUBTREE_ENTRYID)
            .map(|(_, b)| u32le(b, b.len().saturating_sub(4)))
            .filter(|n| self.nodes.contains_key(n))
        {
            return Ok(n);
        }
        // 같은 이름이 둘 이상이면(빈 것이 먼저 나왔다) 하위 폴더가 가장 많은 것.
        let mut best: Option<(usize, u32)> = None;
        let mut level = vec![NID_ROOT_FOLDER];
        for _ in 0..3 {
            let mut next = Vec::new();
            for f in level {
                for (c, p) in self.children(f)? {
                    if text(&p, PR_DISPLAY_NAME)
                        .is_some_and(|n| n.trim().eq_ignore_ascii_case("IPM_SUBTREE"))
                    {
                        let n = self.children(c)?.len();
                        if best.is_none_or(|(m, _)| n > m) {
                            best = Some((n, c));
                        }
                    }
                    next.push(c);
                }
            }
            level = next;
        }
        Ok(best.map_or(NID_ROOT_FOLDER, |(_, c)| c))
    }

    /// 메일 폴더의 메일 항목들(폴더 경로는 IPM 하위 폴더 기준).
    pub fn items(&self) -> Result<Vec<PstItem>, String> {
        let root = self.ipm_subtree()?;
        let mut out = Vec::new();
        let mut stack: Vec<(u32, Vec<String>)> = vec![(root, Vec::new())];
        let mut seen = std::collections::HashSet::new();
        while let Some((folder, path)) = stack.pop() {
            if !seen.insert(folder) {
                continue;
            }
            for (child, fp) in self.children(folder)? {
                if !is_mail_folder(&text(&fp, PR_CONTAINER_CLASS).unwrap_or_default()) {
                    continue;
                }
                let name = text(&fp, PR_DISPLAY_NAME)
                    .filter(|n| !n.trim().is_empty())
                    .unwrap_or_else(|| "폴더".into());
                let mut p = path.clone();
                p.push(name.trim().to_owned());
                stack.push((child, p));
            }
            if path.is_empty() {
                continue;
            }
            let base = folder & !0x1F;
            if let Ok(n) = self.node(base | NID_TYPE_CONTENTS_TABLE) {
                for (msg, _) in self.table(n)? {
                    out.push(PstItem {
                        folder: path.clone(),
                        node: msg,
                    });
                }
            }
        }
        Ok(out)
    }

    /// 진단용: 메일(없으면 메시지 저장소)의 속성 번호·형식·길이, 받는 사람 표의 첫 행 속성.
    pub fn prop_list(&self, nid: Option<u32>) -> Result<Vec<(u16, u16, usize)>, String> {
        let (raw, subs) = self.pc(self.node(nid.unwrap_or(NID_MESSAGE_STORE))?)?;
        let mut out: Vec<(u16, u16, usize)> =
            raw.iter().map(|(&k, (t, b))| (k, *t, b.len())).collect();
        out.sort_unstable();
        if let Some(&n) = subs.get(&NID_RECIPIENT_TABLE)
            && let Some((_, r)) = self.table(n)?.into_iter().next()
        {
            let mut rows: Vec<(u16, u16, usize)> =
                r.iter().map(|(&k, (t, b))| (k, *t, b.len())).collect();
            rows.sort_unstable();
            out.push((0xFFFF, 0, 0));
            out.extend(rows);
        }
        Ok(out)
    }

    /// 진단용: 메일의 첨부마다 (첨부 방식, 첨부 데이터 형식, 크기). 데이터가 없으면 형식 0.
    pub fn attachment_kinds(&self, nid: u32) -> Result<Vec<(i32, u16, usize)>, String> {
        let (_, subs) = self.pc(self.node(nid)?)?;
        let mut out = Vec::new();
        if let Some(&table) = subs.get(&NID_ATTACHMENT_TABLE) {
            for (aid, _) in self.table(table)? {
                let Some(&an) = subs.get(&aid) else {
                    out.push((-1, 0, 0));
                    continue;
                };
                let (araw, _) = self.pc(an)?;
                let method = crate::pst::int(&props(&araw), PR_ATTACH_METHOD).unwrap_or(0);
                let (t, n) = araw
                    .get(&PR_ATTACH_DATA)
                    .map_or((0, 0), |(t, b)| (*t, b.len()));
                out.push((method, t, n));
            }
        }
        Ok(out)
    }

    /// 진단용: 메일의 문자열 속성 하나.
    pub fn prop_text(&self, nid: u32, id: u16) -> Option<String> {
        let (raw, _) = self.pc(self.node(nid).ok()?).ok()?;
        text(&props(&raw), id)
    }

    /// 메일 하나를 EML로. 메일 항목이 아니면(일정·연락처 등) `Ok(None)`.
    pub fn eml(&self, nid: u32) -> Result<Option<Vec<u8>>, String> {
        let (raw, subs) = self.pc(self.node(nid)?)?;
        let p = props(&raw);
        if !crate::pst::is_mail_class(&text(&p, PR_MESSAGE_CLASS).unwrap_or_default()) {
            return Ok(None);
        }
        self.message(&p, &subs, 0).map(Some)
    }

    /// 속성과 하위 노드(받는 사람·첨부 표)로 메일을 만든다. 첨부된 메일은 3단계까지.
    fn message(&self, p: &Props, subs: &SubNodes, depth: u8) -> Result<Vec<u8>, String> {
        let recips: Vec<Props> = match subs.get(&NID_RECIPIENT_TABLE) {
            Some(&n) => self.table(n)?.into_iter().map(|(_, r)| props(&r)).collect(),
            None => Vec::new(),
        };
        let mut parts = Parts::default();
        if let Some(&table) = subs.get(&NID_ATTACHMENT_TABLE) {
            for (aid, _) in self.table(table)? {
                let Some(&an) = subs.get(&aid) else { continue };
                let Ok((araw, asubs)) = self.pc(an) else {
                    continue;
                };
                let ap = props(&araw);
                let method = crate::pst::int(&ap, PR_ATTACH_METHOD).unwrap_or(1);
                match araw.get(&PR_ATTACH_DATA) {
                    // 첨부된 메일: 값은 (노드 번호 4, 크기 4), 그 노드가 첨부의 하위 노드에 있다.
                    Some((PT_OBJECT, obj)) if method == ATTACH_EMBEDDED_MSG && depth < 3 => {
                        let Some(&en) = asubs.get(&u32le(obj, 0)) else {
                            continue;
                        };
                        let Ok((eraw, esubs)) = self.pc(en) else {
                            continue;
                        };
                        let inner = props(&eraw);
                        let raw = self.message(&inner, &esubs, depth + 1)?;
                        parts.message(&inner, raw);
                    }
                    Some((PT_BINARY, data)) => parts.file(&ap, data.clone()),
                    _ => {}
                }
            }
        }
        Ok(build(p, &recips, parts))
    }
}

/// 크기가 정해지지 않은 형식(표에는 HNID로 들어 있다).
fn is_variable(ptype: u16) -> bool {
    matches!(
        ptype,
        PT_OBJECT | PT_STRING8 | PT_UNICODE | PT_BINARY | PT_CLSID
    ) || ptype & 0x1000 != 0
}

/// 읽은 속성 → EML 만들기용 값.
fn props(raw: &RawProps) -> Props {
    raw.iter()
        .filter_map(|(&id, (ptype, b))| {
            let v = match *ptype {
                PT_UNICODE => {
                    let units: Vec<u16> = b
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|c| u16::from_le_bytes(*c))
                        .collect();
                    Val::Text(
                        String::from_utf16_lossy(&units)
                            .trim_end_matches('\0')
                            .to_owned(),
                    )
                }
                PT_STRING8 => Val::Text(string8(b)),
                PT_BINARY => Val::Bin(b.clone()),
                PT_I2 => Val::Int(i32::from(u16le(b, 0) as i16)),
                PT_LONG => Val::Int(u32le(b, 0) as i32),
                PT_BOOLEAN => Val::Int(i32::from(b.first().copied().unwrap_or(0))),
                PT_SYSTIME => Val::Time(filetime_ms(u64le(b, 0) as i64)?),
                _ => return None,
            };
            Some((id, v))
        })
        .collect()
}

/// 노드 데이터의 힙(HN): 블록마다 한 페이지.
struct Heap {
    pages: Vec<Vec<u8>>,
}

impl Heap {
    fn read(ost: &OstFile, bid: u64) -> Result<Self, String> {
        let pages = ost.data(bid)?;
        if pages.first().and_then(|p| p.get(2)) != Some(&0xEC) {
            return Err("힙이 아닙니다".into());
        }
        Ok(Self { pages })
    }

    fn client(&self) -> u8 {
        self.pages[0].get(3).copied().unwrap_or(0)
    }

    fn root(&self) -> u32 {
        u32le(&self.pages[0], 4)
    }

    /// HID(종류 5비트 = 0, 순번 11비트, 페이지 16비트)의 항목.
    fn item(&self, hid: u32) -> Option<&[u8]> {
        if hid & 0x1F != 0 {
            return None;
        }
        let index = ((hid >> 5) & 0x7FF) as usize;
        let page = self.pages.get((hid >> 16) as usize)?;
        let map = usize::from(u16le(page, 0));
        let count = usize::from(u16le(page, map));
        if index == 0 || index > count {
            return None;
        }
        let start = usize::from(u16le(page, map + 4 + (index - 1) * 2));
        let end = usize::from(u16le(page, map + 4 + index * 2));
        page.get(start..end)
    }

    /// BTH의 (열쇠, 값)들.
    fn bth(&self, hid: u32) -> Result<Vec<Record>, String> {
        let h = self.item(hid).ok_or("BTH 머리가 없습니다")?;
        if h.first() != Some(&0xB5) || h.len() < 8 {
            return Err("BTH가 아닙니다".into());
        }
        let (key, ent, levels, root) = (usize::from(h[1]), usize::from(h[2]), h[3], u32le(h, 4));
        let mut out = Vec::new();
        if root == 0 || key == 0 {
            return Ok(out);
        }
        let mut stack = vec![(root, levels)];
        while let Some((hid, level)) = stack.pop() {
            let Some(items) = self.item(hid) else {
                continue;
            };
            if level == 0 {
                for r in items.chunks_exact(key + ent) {
                    out.push((r[..key].to_vec(), r[key..].to_vec()));
                }
            } else {
                // 안쪽 단계는 뒤에서부터 넣어 열쇠 순서대로 나오게 한다.
                for r in items.chunks_exact(key + 4).rev() {
                    stack.push((u32le(r, key), level - 1));
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    //! 4K 형식 파일을 직접 만들어 읽는다: 저장소 → 루트 → IPM_SUBTREE → 받은 편지함 → 메일 한 통(받는 사람 1,
    //! 첨부 1). 메일 속성 블록은 zlib으로 압축해 둔다.
    use std::io::Write;

    use super::*;

    fn unicode(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    /// 힙 하나: 항목들(첫 항목이 사용자 루트).
    fn heap(client: u8, items: &[Vec<u8>]) -> Vec<u8> {
        let mut b = vec![0u8; 12];
        b[2] = 0xEC;
        b[3] = client;
        b[4..8].copy_from_slice(&(1u32 << 5).to_le_bytes());
        let mut offs = vec![12u16];
        for it in items {
            b.extend_from_slice(it);
            offs.push(b.len() as u16);
        }
        let map = b.len() as u16;
        b[0..2].copy_from_slice(&map.to_le_bytes());
        b.extend_from_slice(&(items.len() as u16).to_le_bytes());
        b.extend_from_slice(&0u16.to_le_bytes());
        for o in offs {
            b.extend_from_slice(&o.to_le_bytes());
        }
        b
    }

    fn hid(index: u32) -> u32 {
        index << 5
    }

    /// 속성 문맥: (번호, 형식, 값). 4바이트 이하 형식은 값에, 나머지는 힙 항목으로.
    fn pc(props: &[(u16, u16, Vec<u8>)]) -> Vec<u8> {
        let mut items = vec![Vec::new(), Vec::new()];
        let mut recs = Vec::new();
        let mut sorted = props.to_vec();
        sorted.sort_by_key(|p| p.0);
        for (id, t, v) in sorted {
            recs.extend_from_slice(&id.to_le_bytes());
            recs.extend_from_slice(&t.to_le_bytes());
            if matches!(t, PT_LONG | PT_BOOLEAN) {
                let mut x = v.clone();
                x.resize(4, 0);
                recs.extend_from_slice(&x);
            } else {
                items.push(v);
                recs.extend_from_slice(&hid(items.len() as u32).to_le_bytes());
            }
        }
        items[0] = vec![0xB5, 2, 6, 0];
        items[0].extend_from_slice(&hid(2).to_le_bytes());
        items[1] = recs;
        heap(0xBC, &items)
    }

    /// 표 문맥: 행 번호 + 4바이트 열들(정수 또는 유니코드 글).
    fn tc(cols: &[(u16, u16)], rows: &[(u32, Vec<Vec<u8>>)]) -> Vec<u8> {
        let mut items = vec![Vec::new(), Vec::new()];
        let width = 4 + cols.len() * 4;
        let row_size = width + 1;
        let mut matrix = Vec::new();
        for (id, vals) in rows {
            let mut r = id.to_le_bytes().to_vec();
            for ((_, t), v) in cols.iter().zip(vals) {
                if *t == PT_UNICODE {
                    items.push(v.clone());
                    r.extend_from_slice(&hid(items.len() as u32).to_le_bytes());
                } else {
                    let mut x = v.clone();
                    x.resize(4, 0);
                    r.extend_from_slice(&x);
                }
            }
            r.push(0xFF);
            matrix.extend_from_slice(&r);
        }
        items[1] = matrix;
        let mut info = vec![0x7C, (cols.len() + 1) as u8];
        for v in [width, width, width, row_size] {
            info.extend_from_slice(&(v as u16).to_le_bytes());
        }
        info.extend_from_slice(&0u32.to_le_bytes());
        info.extend_from_slice(&(if rows.is_empty() { 0 } else { hid(2) }).to_le_bytes());
        info.extend_from_slice(&0u32.to_le_bytes());
        let mut desc = |tag: u32, ib: usize, bit: u8| {
            info.extend_from_slice(&tag.to_le_bytes());
            info.extend_from_slice(&(ib as u16).to_le_bytes());
            info.push(4);
            info.push(bit);
        };
        desc((0x67F2 << 16) | u32::from(PT_LONG), 0, 0);
        for (i, (id, t)) in cols.iter().enumerate() {
            desc(
                (u32::from(*id) << 16) | u32::from(*t),
                4 + i * 4,
                (i + 1) as u8,
            );
        }
        items[0] = info;
        heap(0x7C, &items)
    }

    #[test]
    fn reads_4k_ost_folders_and_mail() {
        let store = pc(&[(0x3001, PT_UNICODE, unicode("메일함"))]);
        let folder = |name: &str| pc(&[(PR_DISPLAY_NAME, PT_UNICODE, unicode(name))]);
        let children = |nids: &[u32]| {
            tc(
                &[],
                &nids.iter().map(|&n| (n, Vec::new())).collect::<Vec<_>>(),
            )
        };
        let msg = pc(&[
            (PR_MESSAGE_CLASS, PT_UNICODE, unicode("IPM.Note")),
            (0x0037, PT_UNICODE, unicode("주간 보고")),
            (0x0042, PT_UNICODE, unicode("김철수")),
            (0x0065, PT_UNICODE, unicode("kim@x.com")),
            (
                0x0E06,
                PT_SYSTIME,
                133_000_000_000_000_000i64.to_le_bytes().to_vec(),
            ),
            (
                0x1013,
                PT_BINARY,
                [
                    &b"<p>\xEB\xB3\xB8\xEB\xAC\xB8</p>"[..],
                    &b"<p>a</p>".repeat(300),
                ]
                .concat(),
            ),
            (0x3FDE, PT_LONG, 65001u32.to_le_bytes().to_vec()),
        ]);
        let recips = tc(
            &[
                (0x0C15, PT_LONG),
                (0x3001, PT_UNICODE),
                (0x3003, PT_UNICODE),
            ],
            &[(
                0,
                vec![
                    1u32.to_le_bytes().to_vec(),
                    unicode("이영희"),
                    unicode("lee@x.com"),
                ],
            )],
        );
        let atts = tc(&[], &[(0x8025, Vec::new())]);
        let att = pc(&[
            (PR_ATTACH_METHOD, PT_LONG, 1u32.to_le_bytes().to_vec()),
            (0x3707, PT_UNICODE, unicode("a.txt")),
            (PR_ATTACH_DATA, PT_BINARY, b"hello".to_vec()),
        ]);
        let mut sl = vec![2u8, 0];
        let subs = [
            (NID_RECIPIENT_TABLE, 0x40u64),
            (NID_ATTACHMENT_TABLE, 0x44),
            (0x8025, 0x48),
        ];
        sl.extend_from_slice(&(subs.len() as u16).to_le_bytes());
        sl.extend_from_slice(&[0; 4]);
        for (nid, bid) in subs {
            sl.extend_from_slice(&u64::from(nid).to_le_bytes());
            sl.extend_from_slice(&bid.to_le_bytes());
            sl.extend_from_slice(&0u64.to_le_bytes());
        }
        // (노드, 데이터 블록, 하위 노드 블록)과 (블록, 내용, 압축)
        let nodes: [(u32, u64, u64); 9] = [
            (NID_MESSAGE_STORE, 0x04, 0),
            (NID_ROOT_FOLDER, 0x08, 0),
            (0x12D, 0x0C, 0),
            (0x8022, 0x10, 0),
            (0x802D, 0x14, 0),
            (0x8042, 0x18, 0),
            (0x804E, 0x1C, 0),
            (0x20_0024, 0x20, 0x26),
            (0x804D, 0x2C, 0),
        ];
        let blocks: Vec<(u64, Vec<u8>, bool)> = vec![
            (0x04, store, false),
            (0x08, folder("루트"), false),
            (0x0C, children(&[0x8022]), false),
            (0x10, folder("IPM_SUBTREE"), false),
            (0x14, children(&[0x8042]), false),
            (0x18, folder("받은 편지함"), false),
            (0x1C, children(&[0x20_0024]), false),
            (0x20, msg, true),
            (0x26, sl, false),
            (0x40, recips, false),
            (0x44, atts, false),
            (0x48, att, false),
            (0x2C, children(&[]), false),
        ];
        let mut file = vec![0u8; PAGE * 3];
        let mut bbt = Vec::new();
        for (bid, data, compress) in &blocks {
            let stored = if *compress {
                let mut z =
                    flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
                z.write_all(data).unwrap();
                z.finish().unwrap()
            } else {
                data.clone()
            };
            let ib = file.len() as u64;
            let total = (stored.len() as u64 + BLOCK_TRAILER).div_ceil(BLOCK_ALIGN) * BLOCK_ALIGN;
            let mut blk = stored.clone();
            blk.resize((total - BLOCK_TRAILER) as usize, 0);
            blk.extend_from_slice(&(stored.len() as u16).to_le_bytes());
            blk.extend_from_slice(&[0; 6]);
            blk.extend_from_slice(&bid.to_le_bytes());
            blk.extend_from_slice(&[2, 0]);
            blk.extend_from_slice(&(data.len() as u16).to_le_bytes());
            blk.extend_from_slice(&[0; 4]);
            file.extend_from_slice(&blk);
            let mut e = bid.to_le_bytes().to_vec();
            e.extend_from_slice(&ib.to_le_bytes());
            e.extend_from_slice(&(stored.len() as u16).to_le_bytes());
            e.extend_from_slice(&(data.len() as u16).to_le_bytes());
            e.extend_from_slice(&[1, 0, 0, 0]);
            bbt.push(e);
        }
        let page = |entries: Vec<Vec<u8>>, size: u8, ptype: u8| {
            let mut p = vec![0u8; PAGE];
            for (i, e) in entries.iter().enumerate() {
                p[i * usize::from(size)..i * usize::from(size) + e.len()].copy_from_slice(e);
            }
            p[4056..4058].copy_from_slice(&(entries.len() as u16).to_le_bytes());
            p[4060] = size;
            p[4072] = ptype;
            p[4073] = ptype;
            p
        };
        let nbt: Vec<Vec<u8>> = nodes
            .iter()
            .map(|(nid, data, sub)| {
                let mut e = u64::from(*nid).to_le_bytes().to_vec();
                e.extend_from_slice(&data.to_le_bytes());
                e.extend_from_slice(&sub.to_le_bytes());
                e.extend_from_slice(&[0; 8]);
                e
            })
            .collect();
        file[PAGE..PAGE * 2].copy_from_slice(&page(nbt, 32, PTYPE_NBT));
        file[PAGE * 2..PAGE * 3].copy_from_slice(&page(bbt, 24, PTYPE_BBT));
        file[0..4].copy_from_slice(b"!BDN");
        file[8..10].copy_from_slice(b"SO");
        file[10..12].copy_from_slice(&36u16.to_le_bytes());
        file[224..232].copy_from_slice(&(PAGE as u64).to_le_bytes());
        file[240..248].copy_from_slice(&(PAGE as u64 * 2).to_le_bytes());

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.ims");
        std::fs::write(&path, &file).unwrap();
        let ost = OstFile::open(&path).unwrap();
        let items = ost.items().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].folder, ["받은 편지함"], "IPM_SUBTREE 아래부터");
        let raw = ost.eml(items[0].node).unwrap().unwrap();
        let s = eo_mime::backup::summarize(&raw).unwrap();
        assert_eq!(s.subject, "주간 보고");
        assert_eq!(s.from.unwrap().email, "kim@x.com");
        assert!(s.to.contains("lee@x.com"), "받는 사람: {}", s.to);
        assert!(s.date.is_some());
        let x = eo_mime::extract(&raw).unwrap();
        assert_eq!(x.attachment_names, ["a.txt"]);
        assert!(x.body_text.contains("본문"), "압축된 블록의 HTML 본문");
    }
}
