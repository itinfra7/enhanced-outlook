//! 합성 말뭉치 생성기(계획서 8.4): 한국어 업무 메일 형태, 평균 본문 약 4KB, 주소 5~20개.
//! 같은 번호는 항상 같은 메일을 만든다(정답 확인에 다시 만들어 쓴다).

use crate::MailDoc;

/// 받은 편지함 폴더 번호.
pub const FOLDER_INBOX: u64 = 1;
/// 보낸 편지함 폴더 번호(숨은 참조는 여기만).
pub const FOLDER_SENT: u64 = 3;
/// 일부 메일의 숨은 참조에만 심은 주소.
pub const PLANTED_BCC: &str = "audit.bcc@secure-partner.co.kr";

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        // splitmix64로 섞어 0이 되지 않게 한다.
        let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        Self((z ^ (z >> 31)) | 1)
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn range(&mut self, lo: usize, hi: usize) -> usize {
        lo + self.below(hi - lo + 1)
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }

    fn pick<'a>(&mut self, xs: &[&'a str]) -> &'a str {
        xs[self.below(xs.len())]
    }
}

pub const WORDS: &[&str] = &[
    "보고서",
    "회의",
    "일정",
    "검토",
    "요청",
    "결과",
    "예산",
    "프로젝트",
    "계약",
    "승인",
    "견적",
    "납품",
    "고객",
    "매출",
    "분석",
    "계획",
    "진행",
    "현황",
    "공유",
    "확인",
    "자료",
    "변경",
    "담당자",
    "협조",
    "문의",
    "답변",
    "검수",
    "배포",
    "장애",
    "점검",
    "작업",
    "서버",
    "네트워크",
    "품질",
    "개선",
    "제안",
    "교육",
    "평가",
    "실적",
    "목표",
    "전략",
    "마케팅",
    "영업",
    "구매",
    "정산",
    "세금계산서",
    "회계",
    "감사",
    "보안",
    "정책",
    "규정",
    "시스템",
    "운영",
    "개발",
    "테스트",
    "출시",
    "주간",
    "월간",
    "분기",
    "연간",
    "방안",
    "이슈",
    "리스크",
    "대응",
    "조치",
    "완료",
    "보류",
    "연기",
    "재검토",
    "가이드",
    "매뉴얼",
    "설명회",
    "워크숍",
    "출장",
    "휴가",
    "결재",
    "기안",
    "시행",
    "통보",
    "안내",
    "공지",
    "참석",
    "회신",
    "부탁",
    "수고",
    "조직",
    "부서",
    "본부",
    "센터",
    "팀장",
    "부장",
    "협력사",
    "공급사",
    "인프라",
    "회선",
    "가입자",
    "요금",
    "약정",
    "해지",
    "개통",
    "설치",
    "고장",
    "민원",
    "통계",
    "지표",
    "대시보드",
    "데이터",
    "백업",
    "복구",
    "모니터링",
    "알림",
    "권한",
    "계정",
];
const ENGLISH: &[&str] = &[
    "report", "meeting", "schedule", "review", "budget", "project", "contract", "approval",
    "quote", "delivery", "customer", "sales", "analysis", "plan", "status", "update", "issue",
    "risk", "release", "server", "network", "quality", "training", "audit", "security", "policy",
    "system", "ops", "dev", "test", "kpi", "okr", "q1", "q2", "q3", "q4", "draft", "final", "asap",
    "fyi",
];
const PARTICLES: &[&str] = &[
    "을", "를", "이", "가", "은", "는", "에서", "으로", "와", "의", "에", "도",
];
const ENDINGS: &[&str] = &[
    "합니다.",
    "입니다.",
    "부탁드립니다.",
    "드립니다.",
    "했습니다.",
    "예정입니다.",
    "바랍니다.",
];
const SURNAMES: &[(&str, &str)] = &[
    ("김", "kim"),
    ("이", "lee"),
    ("박", "park"),
    ("최", "choi"),
    ("정", "jung"),
    ("강", "kang"),
    ("조", "cho"),
    ("윤", "yoon"),
    ("장", "jang"),
    ("임", "lim"),
    ("한", "han"),
    ("오", "oh"),
    ("서", "seo"),
    ("신", "shin"),
    ("권", "kwon"),
    ("황", "hwang"),
    ("안", "ahn"),
    ("송", "song"),
];
const GIVEN: &[(&str, &str)] = &[
    ("민수", "minsu"),
    ("지영", "jiyoung"),
    ("서연", "seoyeon"),
    ("도윤", "doyoon"),
    ("하준", "hajun"),
    ("지우", "jiwoo"),
    ("예린", "yerin"),
    ("현우", "hyunwoo"),
    ("수빈", "subin"),
    ("준호", "junho"),
    ("유진", "yujin"),
    ("성민", "sungmin"),
    ("다은", "daeun"),
    ("재훈", "jaehoon"),
    ("소연", "soyeon"),
    ("태현", "taehyun"),
    ("은지", "eunji"),
    ("민재", "minjae"),
    ("혜진", "hyejin"),
    ("동현", "donghyun"),
];
const DOMAINS: &[&str] = &[
    "example.com",
    "example.com",
    "example.com",
    "sk.com",
    "partner.co.kr",
    "vendor-korea.com",
    "example.org",
];
const EXTS: &[&str] = &["pdf", "xlsx", "docx", "pptx", "hwp", "zip"];

fn person(r: &mut Rng) -> String {
    let (sk, se) = SURNAMES[r.below(SURNAMES.len())];
    let (gk, ge) = GIVEN[r.below(GIVEN.len())];
    let domain = r.pick(DOMAINS);
    format!("{sk}{gk} <{ge}.{se}@{domain}>")
}

fn people(r: &mut Rng, n: usize) -> String {
    (0..n).map(|_| person(r)).collect::<Vec<_>>().join("; ")
}

fn sentence(r: &mut Rng) -> String {
    let mut s = String::new();
    for k in 0..r.range(5, 10) {
        if k > 0 {
            s.push(' ');
        }
        if r.chance(8) {
            s.push_str(r.pick(ENGLISH));
        } else if r.chance(6) {
            s.push_str(&format!("{}", r.range(1, 2030)));
        } else {
            s.push_str(r.pick(WORDS));
            if r.chance(70) {
                s.push_str(r.pick(PARTICLES));
            }
        }
    }
    s.push(' ');
    s.push_str(r.pick(ENDINGS));
    s
}

/// 받은 시각: 2024-01-01(한국 시간)부터 `i`에 따라 약 3년에 걸쳐 퍼뜨린다.
fn date(i: u64, n: u64, r: &mut Rng) -> i64 {
    const START: i64 = 1_704_034_800_000; // 2024-01-01T00:00:00+09:00
    const SPAN: i64 = 3 * 365 * 86_400_000;
    START + (SPAN / n.max(1) as i64) * i as i64 + r.below(3_600_000) as i64
}

/// 말뭉치 `n`통 중 `i`번째 메일.
pub fn doc(i: u64, n: u64) -> MailDoc {
    let mut r = Rng::new(i);
    let folder = match r.below(100) {
        0..=44 => FOLDER_INBOX,
        45..=74 => FOLDER_SENT,
        x => 4 + (x as u64 % 10),
    };
    let mut subject = String::new();
    if r.chance(25) {
        subject.push_str("RE: ");
    } else if r.chance(10) {
        subject.push_str("[공지] ");
    }
    if i.is_multiple_of(101) {
        subject.push_str("월간보고서 ");
    }
    let words: Vec<&str> = (0..r.range(2, 5)).map(|_| r.pick(WORDS)).collect();
    subject.push_str(&words.join(" "));

    let mut body = String::from("안녕하세요.\n");
    while body.len() < 3200 + r.below(1600) {
        body.push_str(&sentence(&mut r));
        body.push(if r.chance(20) { '\n' } else { ' ' });
    }
    if r.chance(30) {
        body.push_str(&format!(
            "\n-----Original Message-----\nFrom: {}\nSent: 2025-{:02}-{:02}\nSubject: {}\n{}\n",
            person(&mut r),
            r.range(1, 12),
            r.range(1, 28),
            r.pick(WORDS),
            sentence(&mut r)
        ));
    }
    body.push_str("감사합니다.");

    let to_n = r.range(2, 8);
    let cc_n = r.range(2, 8);
    let mut bcc = String::new();
    if folder == FOLDER_SENT && r.chance(60) {
        let bcc_n = r.range(1, 4);
        bcc = people(&mut r, bcc_n);
    }
    if i.is_multiple_of(997) {
        if !bcc.is_empty() {
            bcc.push_str("; ");
        }
        bcc.push_str(&format!("감사팀 <{PLANTED_BCC}>"));
    }
    let attach_n = r.below(4);
    let attach = (0..attach_n)
        .map(|_| {
            format!(
                "{}년_{}_{}.{}",
                r.range(2023, 2026),
                r.pick(WORDS),
                r.pick(WORDS),
                r.pick(EXTS)
            )
        })
        .collect::<Vec<_>>()
        .join(" ");
    MailDoc {
        id: i + 1,
        folder,
        date: date(i, n, &mut r),
        subject,
        body,
        from: person(&mut r),
        to: people(&mut r, to_n),
        cc: people(&mut r, cc_n),
        bcc,
        attach,
    }
}

/// 질의 1~3단어 `n`개(계획서 8.4: 200종).
pub fn queries(n: usize) -> Vec<String> {
    let mut r = Rng::new(0xC0FFEE);
    (0..n)
        .map(|k| match k % 10 {
            0 => format!(
                "{}{}",
                SURNAMES[r.below(SURNAMES.len())].0,
                GIVEN[r.below(GIVEN.len())].0
            ),
            1 => GIVEN[r.below(GIVEN.len())].1.to_string(),
            2 => format!("제목:{}", r.pick(WORDS)),
            3 => format!("{} {}", r.pick(WORDS), r.pick(WORDS)),
            4 => format!("{} {} {}", r.pick(WORDS), r.pick(WORDS), r.pick(ENGLISH)),
            5 => r.pick(ENGLISH).to_string(),
            6 => format!("보낸이:{}", SURNAMES[r.below(SURNAMES.len())].1),
            7 => format!("\"{} {}\"", r.pick(WORDS), r.pick(WORDS)),
            8 => format!("{} 기간:2025-01-01..2025-06-30", r.pick(WORDS)),
            _ => r.pick(WORDS).to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_and_sized() {
        assert_eq!(doc(42, 1000), doc(42, 1000));
        let avg = (0..200).map(|i| doc(i, 200).body.len()).sum::<usize>() / 200;
        assert!((3000..6000).contains(&avg), "평균 본문 {avg}바이트");
        let addrs = (0..200)
            .map(|i| {
                let d = doc(i, 200);
                [d.to, d.cc, d.bcc]
                    .iter()
                    .map(|s| s.matches('@').count())
                    .sum::<usize>()
            })
            .collect::<Vec<_>>();
        assert!(addrs.iter().all(|&n| (4..=21).contains(&n)), "{addrs:?}");
        assert!(doc(0, 10).bcc.contains(PLANTED_BCC));
    }
}
