//! `owa-cli`: 웹메일 프로토콜 개발·검증용 명령줄 도구(계획서 10장 1단계).
//! 아이디·비밀번호는 실행할 때마다 터미널에서 입력받아 메모리에만 둔다. 메일 제목·주소는 출력하지 않는다.

mod changes;
mod domino_cmd;
mod dump;
mod probe;
mod probe_v070;
mod prompt;
mod store_cmd;
mod synth_cmd;
mod util;
mod verify;
mod watch;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use eo_owa::{ClientConfig, FolderRef, PostMode};

use crate::util::{fetch_hierarchy, fmt_bytes, item_sync, ms, resolve_folder};

#[derive(Parser)]
#[command(
    name = "owa-cli",
    version,
    about = "Outlook 웹메일 프로토콜 개발·검증 도구"
)]
struct Cli {
    /// Outlook 웹메일 서버 주소(로그인하는 명령에 필요, 예: mail.example.com)
    #[arg(long, global = true)]
    base_url: Option<String>,
    /// 요청 JSON 전달 방식: auto = 본문, 형식 오류면 헤더로 재시도
    #[arg(long, global = true, value_enum, default_value_t = Mode::Auto)]
    post_mode: Mode,
    /// NTLM 채널 바인딩(확장 보호)을 끈다 (진단용)
    #[arg(long, global = true)]
    no_channel_binding: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Copy, ValueEnum)]
enum Mode {
    Auto,
    Body,
    Header,
}

impl From<Mode> for PostMode {
    fn from(m: Mode) -> Self {
        match m {
            Mode::Auto => PostMode::Auto,
            Mode::Body => PostMode::Body,
            Mode::Header => PostMode::Header,
        }
    }
}

#[derive(Subcommand)]
enum Command {
    /// 로그인 확인
    Login,
    /// 메일 폴더 목록
    Folders,
    /// 폴더 항목 동기화 (--state 파일이 있으면 그 상태부터 증분)
    Sync {
        /// inbox 같은 잘 알려진 이름, 폴더 이름, 또는 "상위 / 하위" 경로
        folder: String,
        /// SyncState를 읽고 쓸 파일
        #[arg(long)]
        state: Option<PathBuf>,
    },
    /// 원문(EML) 덤프
    DumpEml {
        folder: String,
        out: PathBuf,
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long, default_value_t = 4)]
        concurrency: usize,
        #[arg(long, default_value_t = 4)]
        batch: usize,
    },
    /// 메일함 용량
    Quota,
    /// 고정(핀) 속성 확인(읽기만, 1.1.0): RenewTime·ReceivedOrRenewTime·0x0F01·0x0F02를 여러 꼴로 요청해 본다
    Pins {
        #[arg(default_value = "inbox")]
        folder: String,
        #[arg(long, default_value_t = 40)]
        max: u32,
    },
    /// 읽기 전용 실서버 점검: 로그인 → 계층 → 받은 편지함 증분 → 원문 1건
    Smoke,
    /// 1단계 읽기 전용 확인 전체 (로그인 1회)
    Verify {
        /// 덤프·보고서 위치 (기본 실행 파일 폴더의 EnhancedOutlook\dev\dump-시각)
        #[arg(long)]
        dump_dir: Option<PathBuf>,
        #[arg(long)]
        skip_dump: bool,
    },
    /// 유휴 간격(분)을 늘려 가며 세션 만료 측정
    Watch {
        #[arg(long, value_delimiter = ',', default_values_t = [15u64, 30, 60, 120, 240, 480])]
        gaps: Vec<u64>,
    },
    /// 전 메일 폴더 변경 감시: 웹메일에서 한 읽음·이동·삭제의 변경 종류 확인
    Changes {
        /// 확인 간격(초)
        #[arg(long, default_value_t = 5)]
        interval: u64,
    },
    /// 메일함을 바꾸는 확인(11장 #5~#10). 본인에게 보낸 테스트 메일로만, 실행 전 y/N 확인
    Probe {
        #[command(subcommand)]
        what: ProbeCommand,
    },
    /// HCL Domino 웹메일 확인(아이디·비밀번호는 터미널에서). 기본은 메일함별 통수만(메일을 열지 않음),
    /// --root를 주면 그 저장소에 가져온다('new'였던 메일은 연 직후 안읽음으로 되돌린다)
    Domino {
        /// HCL Domino 포털 주소(예: portal.example.com)
        #[arg(long)]
        portal: String,
        /// 가져와 넣을 저장 위치
        #[arg(long)]
        root: Option<PathBuf>,
        /// 이 메일함만 (XML_Sent 등)
        #[arg(long)]
        view: Option<String>,
        /// 최대 가져올 수
        #[arg(long)]
        max: Option<usize>,
        /// 자동 정리 삭제 확인(메일함 변경, 사용자 허락 뒤에만): 제목에 이 표시가 든 받은 메일 한 통을
        /// 지운 메일로 보낸 뒤 지운 메일에서도 지운다
        #[arg(long)]
        delete_test: Option<String>,
        /// 양방향 연동 동작 확인(메일함 변경, 사용자 허락 뒤에만): 제목에 이 표시가 든 받은 메일 한 통으로
        /// 읽음·안읽음, ☆ 켜기·끄기, 지운 메일로·되살리기 뒤 처음 상태로 둔다
        #[arg(long)]
        action_test: Option<String>,
        /// 메일 문서의 숨은 필드를 모두 보인다(읽기만, UNID 여러 개는 쉼표로). 이미 읽은 메일로만
        /// (문서를 열면 서버가 'new' 표시를 지운다)
        #[arg(long, value_delimiter = ',')]
        fields: Vec<String>,
    },
    /// 로컬 저장소·동기화 엔진(2단계)
    Store {
        /// 저장 위치 (기본 실행 파일 폴더의 EnhancedOutlook)
        #[arg(long)]
        root: Option<PathBuf>,
        #[command(subcommand)]
        what: StoreCommand,
    },
}

#[derive(Subcommand)]
enum StoreCommand {
    /// 로컬 현황(로그인 없음)
    Status,
    /// 원문은 있고 색인되지 않은 로컬 메일 색인(로그인 없음). --check면 DB와 색인을 대조해 고친다
    Index {
        #[arg(long)]
        check: bool,
    },
    /// 로컬 검색(로그인 없음). 예: "보고서", "제목:회의 기간:2026-01-01..2026-03-31"
    Search {
        query: String,
        /// 이 폴더만(폴더 이름)
        #[arg(long)]
        folder: Option<String>,
        /// 관련도순(기본 최신순)
        #[arg(long)]
        relevance: bool,
        /// 제목·보낸 사람도 표시
        #[arg(long)]
        show: bool,
    },
    /// 메일 상태만 보기(로그인 없음): 폴더·읽음·플래그·서버 상태. 제목·주소는 출력하지 않는다
    Show { id: i64 },
    /// 스레드 확인(로그인 없음): 원문의 In-Reply-To·References가 가리키는 로컬 메일 번호·폴더, 첨부 수
    Thread { id: i64 },
    /// 화면 성능 측정용 합성 저장소 만들기(로그인 없음, 4단계). 비어 있는 --root에만 만든다
    Synth {
        /// 받은 편지함 메일 수(10만 행 목록)
        #[arg(long, default_value_t = 100_000)]
        inbox: usize,
        /// 나머지 폴더 메일 수
        #[arg(long, default_value_t = 20_000)]
        others: usize,
        /// 원문을 둘 최근 메일 수
        #[arg(long, default_value_t = 5_000)]
        eml: usize,
    },
    /// EML 폴더·ZIP, 유저스크립트 HTML 백업(폴더·ZIP)을 로컬 전용 메일로 가져온다(로그인 없음, 7단계)
    Import {
        /// 폴더, ZIP, EML·HTML 파일(여러 개). HCL Domino 백업은 받은 메일·보낸 메일 등이 든 폴더
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// 저장하지 않고 해석·복원만 해 본다(폴더별 통수·없는 파일·크기 확인)
        #[arg(long)]
        dry_run: bool,
    },
    /// 본문 그림 참조 진단(로그인 없음): 종류·서버 이름·외부 이미지 허용 여부만, 제목·주소·경로는 출력하지 않는다
    Images {
        /// 메일 번호
        #[arg(long)]
        id: Option<i64>,
        /// 제목에 이 글이 든 메일(최근 5통)
        #[arg(long)]
        subject: Option<String>,
        /// 원문 있는 메일 전체에서 cid 그림이 들어 있는/빠진 메일 수와, 외부 그림 서버별 메일 수
        #[arg(long)]
        scan: bool,
        /// (진단용, --id와 함께) 외부 그림 주소를 한 줄씩 내보낸다. 화면에 띄우지 말고 변수로 받아 쓴다.
        #[arg(long, hide = true)]
        urls: bool,
        /// (진단용, --id와 함께) 앱이 그리는 본문에서 그림 주변 구조: 태그·스타일만, 글자와 주소는 가린다
        #[arg(long)]
        tags: bool,
        /// (진단용) 이 서버의 그림 주소 형태(경로의 고정 낱말·질의 이름만)와 메일 수·예
        #[arg(long)]
        host: Option<String>,
        /// (진단용) 본문 그림(cid)의 실제 형식(PNG·JPEG·TIFF·EMF 등)과 화면에 못 그리는 것의 예
        #[arg(long)]
        formats: bool,
    },
    /// 중복 메일(제목·본문·보낸 사람·받는 사람·참조가 같은 메일) 찾기 시간과 묶음 수(로그인 없음, 지우지 않음)
    Dupes {
        /// 최근 묶음 몇 개를 보일지
        #[arg(long, default_value_t = 5)]
        show: usize,
        /// 두 메일 원문의 다른 점만 보인다(글 해시·Date·Message-ID·링크)
        #[arg(long, num_args = 2)]
        diff: Option<Vec<i64>>,
    },
    /// HCL Domino 리다이렉트 사본 찾기(1.1.0, 로그인 없음, 지우지 않음): 짝 수·건너뛴 사유와 목록
    Redirects {
        /// 목록을 몇 통까지 보일지
        #[arg(long, default_value_t = 200)]
        show: usize,
        /// (진단용) 이 메일들의 본문 글을 이 폴더에 `번호.txt`로 쓴다
        #[arg(long, value_delimiter = ',')]
        dump: Vec<i64>,
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// OST 4K(HCL Traveler .ims) 읽기 확인(로그인 없음, 저장 안 함): 폴더별 통수·EML 만든 수·실패·도메인만
    Ims {
        path: PathBuf,
        /// 앞에서 이만큼만 EML로 만들어 본다
        #[arg(long)]
        limit: Option<usize>,
    },
    /// 가져온 메일의 변환 품질(로그인 없음): 출처(import·html·domino·domino-eml·domino-pst)별 통수만
    Quality {
        #[arg(long, default_value = "domino-pst")]
        origin: String,
    },
    /// 점검용 유저스크립트 형식 HTML 백업 ZIP 견본을 만든다(로그인 없음)
    SampleBackup { out: PathBuf },
    /// 점검용 HCL Domino 백업 형식 견본 폴더를 만든다(로그인 없음, 비어 있는 폴더에만)
    SampleDomino { out: PathBuf },
    /// 계층 → 항목 → 원문 동기화 1주기 (--watch면 반복)
    Sync {
        /// 원문을 이 통수까지만 받고 멈춤(이어받기 확인용)
        #[arg(long)]
        eml_limit: Option<usize>,
        /// 이 간격(초)으로 반복하며 바뀐 것을 출력
        #[arg(long)]
        watch: Option<u64>,
        /// 반복 시간(분), 지나면 자동 종료
        #[arg(long, default_value_t = 5)]
        minutes: u64,
    },
}

#[derive(Subcommand)]
enum ProbeCommand {
    /// #5: 테스트 메일 복사본으로 CopyItem → MoveItem → DeleteItem(지운 편지함) → DeleteItem(SoftDelete)
    Items {
        /// 테스트 메일 제목에 들어 있는 표시
        #[arg(long, default_value = "Outlook-TEST")]
        marker: String,
        /// 복사 대상 폴더
        #[arg(long, default_value = "할 일")]
        copy_to: String,
        /// 이동 대상 폴더
        #[arg(long, default_value = "숙지")]
        move_to: String,
        /// 복사본을 하나 더 만들어 HardDelete(영구 삭제)도 확인
        #[arg(long)]
        hard_delete: bool,
    },
    /// #7: 회신 초안(발송 안 함) → 정리, #6: 전체 회신을 본인에게 1통 발송(받는 사람이 본인뿐일 때만)
    Replies {
        #[arg(long, default_value = "Outlook-TEST")]
        marker: String,
    },
    /// #8 인라인 이미지 초안, #9 MIME 초안(둘 다 발송 없이 정리), #10 스레드 머리글 새 메일을 본인에게 1통 발송
    Drafts {
        #[arg(long, default_value = "Outlook-TEST")]
        marker: String,
    },
    /// 0.7.0: 임시 보관함 저장·첨부 바꾸기·본인에게 보내기, 받은 편지함 아래 시험 폴더 만들기·이름 바꾸기·이동·
    /// 모두 읽음·비우기·삭제(대화에서 허락받은 뒤에만)
    V070 {
        #[arg(long, default_value = "")]
        marker: String,
        /// 시험 저장소(실제 앱 데이터와 따로)
        #[arg(long, default_value = "target/v070-try")]
        root: std::path::PathBuf,
        /// 지난 실행의 시험 저장소에서 남은 작업(보내기 등)만 이어서(새 초안·폴더는 만들지 않음)
        #[arg(long)]
        resume: Option<std::path::PathBuf>,
        /// 지난 실행의 시험 폴더 이름 바꾸기만 다시 확인
        #[arg(long)]
        rename_check: bool,
        /// --rename-check 끝에 시험 폴더를 완전히 지움(되돌릴 수 없음)
        #[arg(long)]
        cleanup: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("오류: 런타임 시작 실패: {e}");
            return ExitCode::FAILURE;
        }
    };
    let code = match runtime.block_on(run(cli)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("오류: {e:#}");
            ExitCode::FAILURE
        }
    };
    // 터미널 탭은 아래쪽이 가려질 수 있어, 끝났음을 화면 가득(20줄 넘게) 보여 준다(출력을 파이프로 받을 때는 생략).
    if std::io::IsTerminal::is_terminal(&std::io::stdout()) {
        println!();
        for _ in 0..22 {
            println!("=== 끝 (더 입력할 것 없음) ===");
        }
    }
    code
}

async fn run(cli: Cli) -> Result<()> {
    // HCL Domino는 따로 로그인한다.
    if let Command::Domino {
        portal,
        root,
        view,
        max,
        delete_test,
        action_test,
        fields,
    } = &cli.command
    {
        return domino_cmd::run(
            portal,
            root.as_deref(),
            view.clone(),
            *max,
            delete_test.clone(),
            action_test.clone(),
            fields,
        )
        .await;
    }
    // 로그인이 필요 없는 명령(로컬 데이터만).
    if let Command::Store { root, what } = &cli.command {
        let root = root.clone().unwrap_or_else(eo_store::default_root);
        match what {
            StoreCommand::Status => return store_cmd::status(&root),
            StoreCommand::Index { check } => return store_cmd::index(&root, *check),
            StoreCommand::Search {
                query,
                folder,
                relevance,
                show,
            } => return store_cmd::search(&root, query, folder.as_deref(), *relevance, *show),
            StoreCommand::Synth { inbox, others, eml } => {
                return synth_cmd::synth(&root, *inbox, *others, *eml);
            }
            StoreCommand::Show { id } => return store_cmd::show(&root, *id),
            StoreCommand::Thread { id } => return store_cmd::thread(&root, *id),
            StoreCommand::Import { paths, dry_run } => {
                return store_cmd::import(&root, paths, *dry_run);
            }
            StoreCommand::Images {
                id,
                subject,
                scan,
                urls,
                tags,
                host,
                formats,
            } => {
                if *scan {
                    return store_cmd::images_scan(&root);
                }
                if let Some(h) = host {
                    if *urls {
                        return store_cmd::host_urls(&root, h, 2);
                    }
                    return store_cmd::image_host(&root, h);
                }
                if *formats {
                    return store_cmd::image_formats(&root);
                }
                if let (true, Some(id)) = (*tags, id) {
                    return store_cmd::image_tags(&root, *id);
                }
                if let (true, Some(id)) = (*urls, id) {
                    return store_cmd::image_urls(&root, *id);
                }
                return store_cmd::images(&root, *id, subject.as_deref());
            }
            StoreCommand::Quality { origin } => {
                return store_cmd::quality(&root, origin);
            }
            StoreCommand::Dupes { show, diff } => {
                return store_cmd::dupes(&root, *show, diff.as_ref().map(|d| (d[0], d[1])));
            }
            StoreCommand::Redirects { show, dump, out } => {
                let store = eo_store::Store::open(&root)?;
                if let Some(out) = out {
                    for id in dump {
                        let row = store.message(*id)?.context("메일 없음")?;
                        let raw = store
                            .eml()
                            .get(row.eml_hash.as_deref().context("원문 없음")?)?;
                        let x = eo_mime::extract(&raw).context("해석 실패")?;
                        std::fs::write(
                            out.join(format!("{id}.txt")),
                            format!("{:?}\n----\n{}", x.attachment_names, x.body_text),
                        )?;
                    }
                    return Ok(());
                }
                let t = Instant::now();
                let s = eo_sync::find_redirect_copies(&store)?;
                println!(
                    "HCL Domino 받은·외부 메일 {}통: 리다이렉트 사본 {}통 · 짝 여럿 {} · 본문 다름 {} · 원문 없음 {} ({}ms)",
                    s.scanned,
                    s.copies.len(),
                    s.ambiguous,
                    s.body_differs,
                    s.without_body,
                    ms(t.elapsed())
                );
                for (d, o, sim, names) in s.differing.iter().take(*show) {
                    println!("본문 다름: #{d} ↔ #{o} 비슷함 {sim:.3} 첨부 이름 같음 {names}");
                }
                for c in s.copies.iter().take(*show) {
                    let o = store.message(c.outlook_message_id)?;
                    println!(
                        "#{} {:.3} {} {} | {} ↔ Outlook #{} {}",
                        c.domino.message_id,
                        c.similarity,
                        c.domino.box_key,
                        c.domino.from_addr,
                        c.domino.subject.chars().take(40).collect::<String>(),
                        c.outlook_message_id,
                        o.map_or(0, |r| (r.props.date_received - c.domino.date) / 1000)
                    );
                }
                return Ok(());
            }
            StoreCommand::Ims { path, limit } => return store_cmd::ims(path, *limit),
            StoreCommand::SampleBackup { out } => {
                let n = eo_sync::write_sample_html_backup(out)?;
                println!("견본 {n}통 → {}", out.display());
                return Ok(());
            }
            StoreCommand::SampleDomino { out } => {
                if out.exists() && std::fs::read_dir(out)?.next().is_some() {
                    bail!(
                        "{}이(가) 비어 있지 않습니다(덮어쓰지 않습니다)",
                        out.display()
                    );
                }
                let n = eo_sync::write_sample_domino_backup(out)?;
                println!("HCL Domino 견본 {n}통 → {}", out.display());
                return Ok(());
            }
            StoreCommand::Sync { .. } => {}
        }
    }
    let base_url = cli
        .base_url
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("--base-url로 웹메일 서버 주소를 주세요"))?;
    let mut cfg = ClientConfig::new(base_url)?;
    cfg.post_mode = cli.post_mode.into();
    cfg.channel_binding = !cli.no_channel_binding;
    let (client, info) = prompt::login(cfg).await?;
    eprintln!("로그인 성공: {:?}, {}ms", info.scheme, ms(info.elapsed));

    match cli.command {
        Command::Login => {
            println!(
                "쿠키: {} / 캐너리 {}",
                info.cookie_names.join(", "),
                if info.has_canary { "있음" } else { "없음" }
            );
        }
        Command::Folders => {
            let h = fetch_hierarchy(&client).await?;
            for n in h.mail_tree() {
                println!(
                    "{}{} ({}통, 안읽음 {})",
                    "  ".repeat(n.depth),
                    n.folder.display_name,
                    n.folder.total_count.unwrap_or(0),
                    n.folder.unread_count.unwrap_or(0)
                );
            }
        }
        Command::Sync { folder, state } => {
            let (folder, name) = resolve_folder(&client, &folder).await?;
            let start_state = match &state {
                Some(p) if p.exists() => Some(std::fs::read_to_string(p)?.trim().to_owned()),
                _ => None,
            };
            let kind = if start_state.is_some() {
                "증분"
            } else {
                "최초"
            };
            let l = item_sync(&client, &folder, start_state.as_deref()).await?;
            let mut types: BTreeMap<&str, usize> = BTreeMap::new();
            for c in &l.changes {
                *types.entry(c.type_name.as_str()).or_default() += 1;
            }
            println!(
                "{name} {kind} 동기화: 변경 {}건, {}ms ({}회), TotalCount {}, 종류 {types:?}",
                l.changes.len(),
                ms(l.elapsed),
                l.calls,
                l.total_count.map_or("?".into(), |n| n.to_string())
            );
            let started = Instant::now();
            let again = item_sync(&client, &folder, Some(&l.sync_state)).await?;
            println!(
                "곧바로 증분: 변경 {}건, {}ms",
                again.changes.len(),
                ms(started.elapsed())
            );
            if let Some(p) = &state {
                std::fs::write(p, &again.sync_state)
                    .with_context(|| format!("{} 쓰기 실패", p.display()))?;
                println!("SyncState 저장: {}", p.display());
            }
        }
        Command::DumpEml {
            folder,
            out,
            limit,
            concurrency,
            batch,
        } => {
            let (folder, name) = resolve_folder(&client, &folder).await?;
            let l = item_sync(&client, &folder, None).await?;
            let mut items = dump::items_from_changes(&l.changes);
            if let Some(n) = limit {
                items.sort_by(|a, b| b.received.cmp(&a.received));
                items.truncate(n);
            }
            println!("{name}: {}통 받는 중 → {}", items.len(), out.display());
            let s =
                dump::fetch_items(&client, &items, Some(&out), batch, concurrency, true).await?;
            println!("{}", s.summary());
            if !s.failures.is_empty() {
                println!("실패: {:?}", s.failures);
            }
        }
        Command::Pins { folder, max } => {
            let (folder, name) = resolve_folder(&client, &folder).await?;
            let fid = eo_owa::requests::folder_id(&folder);
            let ext = |tag: &str| {
                serde_json::json!({
                    "__type": "ExtendedPropertyUri:#Exchange",
                    "PropertyTag": tag,
                    "PropertyType": "SystemTime"
                })
            };
            let uri =
                |f: &str| serde_json::json!({ "__type": "PropertyUri:#Exchange", "FieldURI": f });
            let variants: Vec<(&str, Vec<serde_json::Value>)> = vec![
                ("FieldURI RenewTime", vec![uri("Subject"), uri("RenewTime")]),
                (
                    "FieldURI ReceivedOrRenewTime",
                    vec![uri("Subject"), uri("ReceivedOrRenewTime")],
                ),
                (
                    "Extended 0x0F01·0x0F02",
                    vec![uri("Subject"), ext("0x0F01"), ext("0x0F02")],
                ),
            ];
            println!("{name}: 최신 {max}통");
            for (label, props) in variants {
                let body = serde_json::json!({
                    "__type": "FindItemRequest:#Exchange",
                    "ItemShape": {
                        "__type": "ItemResponseShape:#Exchange",
                        "BaseShape": "IdOnly",
                        "AdditionalProperties": props
                    },
                    "ParentFolderIds": [fid.clone()],
                    "Traversal": "Shallow",
                    "Paging": {
                        "__type": "IndexedPageView:#Exchange",
                        "BasePoint": "Beginning",
                        "Offset": 0,
                        "MaxEntriesReturned": max
                    },
                    "ViewFilter": "All",
                    "SortOrder": [{
                        "__type": "SortResults:#Exchange",
                        "Order": "Descending",
                        "Path": { "__type": "PropertyUri:#Exchange", "FieldURI": "DateTimeReceived" }
                    }]
                });
                println!("== {label}");
                match client.call("FindItem", body).await {
                    Err(e) => println!("  오류: {e}"),
                    Ok(v) => {
                        let items = v
                            .pointer("/Body/ResponseMessages/Items/0/RootFolder/Items")
                            .and_then(|x| x.as_array())
                            .cloned()
                            .unwrap_or_default();
                        if items.is_empty() {
                            let s = v.to_string();
                            println!("  항목 없음: {}", s.chars().take(600).collect::<String>());
                        }
                        for it in items {
                            let subj: String = it
                                .get("Subject")
                                .and_then(|s| s.as_str())
                                .unwrap_or("")
                                .chars()
                                .take(30)
                                .collect();
                            let mut extra = it.clone();
                            if let Some(o) = extra.as_object_mut() {
                                for k in ["__type", "ItemId", "Subject"] {
                                    o.remove(k);
                                }
                            }
                            println!("  {subj} | {extra}");
                        }
                    }
                }
            }
        }
        Command::Quota => {
            let c = client.user_config().await?;
            let used = c.quota_used.unwrap_or(0).max(0) as u64;
            let limit = c.quota_send.unwrap_or(0).max(0) as u64;
            println!(
                "사용 {} / 경고 {} / 발송 제한 {} ({:.0}%)",
                fmt_bytes(used),
                c.quota_warning
                    .map_or("?".into(), |w| fmt_bytes(w.max(0) as u64)),
                fmt_bytes(limit),
                if limit > 0 {
                    used as f64 * 100.0 / limit as f64
                } else {
                    0.0
                }
            );
        }
        Command::Smoke => {
            let h = fetch_hierarchy(&client).await?;
            println!(
                "계층: 메일 폴더 {}개 ({}ms)",
                h.mail_tree().len(),
                ms(h.elapsed)
            );
            let inbox = FolderRef::distinguished("inbox");
            let full = item_sync(&client, &inbox, None).await?;
            let started = Instant::now();
            let inc = item_sync(&client, &inbox, Some(&full.sync_state)).await?;
            println!(
                "받은 편지함: {}통, 증분 변경 {}건 ({}ms)",
                full.changes.len(),
                inc.changes.len(),
                ms(started.elapsed())
            );
            let items = dump::items_from_changes(&full.changes);
            let Some(first) = items.first() else {
                println!("원문: 받은 편지함이 비어 건너뜀");
                return Ok(());
            };
            let mime = client.get_mime(std::slice::from_ref(&first.id)).await?;
            match mime.into_iter().next() {
                Some(Ok(bytes)) => println!("원문 1건: {}", fmt_bytes(bytes.len() as u64)),
                Some(Err(e)) => bail!("원문 실패: {e}"),
                None => bail!("원문 응답 없음"),
            }
            println!("smoke 통과");
        }
        Command::Verify {
            dump_dir,
            skip_dump,
        } => {
            verify::run(&client, &info, dump_dir, skip_dump).await?;
        }
        Command::Watch { gaps } => watch::run(&client, &gaps).await?,
        Command::Changes { interval } => changes::run(&client, interval.max(1)).await?,
        Command::Probe { what } => match what {
            ProbeCommand::Items {
                marker,
                copy_to,
                move_to,
                hard_delete,
            } => probe::items(&client, &marker, &copy_to, &move_to, hard_delete).await?,
            ProbeCommand::Replies { marker } => probe::replies(&client, &marker).await?,
            ProbeCommand::Drafts { marker } => probe::drafts(&client, &marker).await?,
            ProbeCommand::V070 {
                marker,
                root,
                resume,
                rename_check,
                cleanup,
            } => match (resume, rename_check) {
                (Some(dir), _) => probe_v070::resume(&client, &dir).await?,
                (None, true) => probe_v070::rename_check(&client, &marker, cleanup).await?,
                (None, false) => probe_v070::run(&client, &marker, &root).await?,
            },
        },
        Command::Store { root, what } => {
            let root = root.unwrap_or_else(eo_store::default_root);
            match what {
                StoreCommand::Status
                | StoreCommand::Index { .. }
                | StoreCommand::Search { .. }
                | StoreCommand::Show { .. }
                | StoreCommand::Thread { .. }
                | StoreCommand::Import { .. }
                | StoreCommand::Images { .. }
                | StoreCommand::Quality { .. }
                | StoreCommand::Dupes { .. }
                | StoreCommand::Redirects { .. }
                | StoreCommand::Ims { .. }
                | StoreCommand::SampleBackup { .. }
                | StoreCommand::SampleDomino { .. }
                | StoreCommand::Synth { .. } => {
                    unreachable!("로그인 없이 위에서 처리")
                }
                StoreCommand::Sync {
                    eml_limit,
                    watch,
                    minutes,
                } => store_cmd::sync(client, &root, eml_limit, watch, minutes).await?,
            }
        }
        Command::Domino { .. } => unreachable!("웹메일 로그인 없이 위에서 처리"),
    }
    Ok(())
}
