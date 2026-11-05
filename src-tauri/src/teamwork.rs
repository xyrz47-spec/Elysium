//! 병렬 작업: 작업(만들기)을 여러 에이전트에게 나눠 맡긴다.
//! 에이전트마다 작업 폴더의 사본에서 따로 일하고, 끝나면 바뀐 파일만 원래 폴더로 합친다.
//! 두 에이전트가 같은 파일을 다르게 고쳤으면(충돌) 통합 AI가 원래 폴더에서 하나로 합친다.
//! 복사하기에 너무 큰 git 저장소는 사본 대신 git worktree에서 일한다.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// 사본에 넣지 않는 폴더(의존성·빌드 산출물·VCS). 에이전트가 사본에서 설치·빌드해도 합치지 않는다.
const SKIP_DIRS: &[&str] = &[
    ".git", ".hg", ".svn", "node_modules", "target", "dist", "build", "out", ".next", ".nuxt", ".svelte-kit", ".turbo", ".cache", ".parcel-cache", ".venv", "venv", "env", "__pycache__", ".pytest_cache", ".mypy_cache", ".gradle", ".idea", ".vs", "bin", "obj", "coverage", ".elysium-team", ".elysium",
];
/// 이보다 크면 사본을 만들지 않고 한 에이전트로 돌아간다.
const MAX_FILES: usize = 20_000;
const MAX_BYTES: u64 = 300 * 1024 * 1024;
const LIMITS: Limits = Limits { files: MAX_FILES, bytes: MAX_BYTES };
/// 통합 AI에게 글로 보여 주는 충돌 파일 한 판의 최대 글자 수(넘으면 경로만 준다)
const CONFLICT_CHARS: usize = 12_000;
/// 나누는 AI에게 보여 주는 파일 목록의 최대 줄 수
const LISTING_LINES: usize = 300;

/// 상대 경로(슬래시) → 내용 해시
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    pub files: BTreeMap<String, [u8; 32]>,
    pub bytes: u64,
    pub scope: Scope,
}

/// 어떤 파일을 사본에 넣는가
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Scope {
    /// SKIP_DIRS를 뺀 전부
    #[default]
    All,
    /// .gitignore에 걸린 파일까지 뺀다. 사본에는 .git이 없어도 .gitignore는 들어가므로 같은 규칙으로 읽힌다.
    GitIgnore,
}

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub files: usize,
    pub bytes: u64,
}

/// 사본에서 일하는 AI에게 알리는 말(Scope::GitIgnore일 때만)
pub const GITIGNORE_NOTE: &str = "- 폴더가 커서 .gitignore에 걸린 파일(캐시·실행 폴더 등)은 작업 공간에 넣지 않았습니다. 없다고 새로 만들지 마세요.\n";
/// git worktree에서 일하는 AI에게 알리는 말
pub const WORKTREE_NOTE: &str = "- 이 작업 공간은 git worktree입니다. 커밋·브랜치·stash를 만들지 마세요. 끝나면 바뀐 파일만 합칩니다.\n";

/// 사본에서 일하는 AI에게 알리는 말: 사본은 저장소 안에 있어 git 명령이 원래 저장소를 건드린다.
pub const COPY_NOTE: &str = "- 이 작업 공간은 사본입니다. git 명령(commit·add·reset·checkout·stash 등)을 쓰지 마세요. 상위 폴더의 원래 저장소를 건드립니다.\n";

/// 병렬 작업 공간의 기본 위치: 프로젝트의 .elysium/worktrees
pub fn workspace_home(root: &Path) -> PathBuf {
    root.join(".elysium").join("worktrees")
}

/// 에이전트 작업 공간을 어떻게 만들었는가
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prepared {
    Copy,
    Worktree,
}

#[derive(Debug)]
pub struct AgentDirs {
    pub dirs: Vec<PathBuf>,
    pub how: Prepared,
}

fn skip_dir(name: &str) -> bool {
    SKIP_DIRS.iter().any(|d| d.eq_ignore_ascii_case(name))
}

/// Invariant: a worktree's or submodule's `.git` is a file; it must never enter a snapshot or it gets merged into the root.
fn is_git_meta(name: &str) -> bool {
    name.eq_ignore_ascii_case(".git")
}

pub(crate) fn hash_file(p: &Path) -> std::io::Result<[u8; 32]> {
    let data = std::fs::read(p)?;
    Ok(Sha256::digest(&data).into())
}

/// 폴더 안 파일의 해시 목록. 전부 넣기에 너무 크면 .gitignore에 걸린 파일을 빼고 다시 센다. 그래도 크면 Err(사람이 읽는 이유).
pub fn snapshot(root: &Path) -> Result<Snapshot, String> {
    snapshot_within(root, LIMITS)
}

fn snapshot_within(root: &Path, limits: Limits) -> Result<Snapshot, String> {
    walk_all(root, limits).or_else(|_| walk_gitignore(root, limits))
}

/// 사본을 기준과 같은 범위로 다시 센다. 범위가 다르면 무시된 산출물이 새 파일로 잡혀 원래 폴더로 합쳐진다.
pub fn snapshot_like(root: &Path, scope: Scope) -> Result<Snapshot, String> {
    match scope {
        Scope::All => walk_all(root, LIMITS),
        Scope::GitIgnore => walk_gitignore(root, LIMITS),
    }
}

/// 해시하기 전에 크기만 먼저 센다. 한도를 넘는 폴더를 수백 MB씩 읽고 나서야 포기하지 않게 한다.
#[derive(Default)]
struct Found {
    files: Vec<PathBuf>,
    bytes: u64,
}

impl Found {
    fn add(&mut self, path: PathBuf, len: u64, limits: Limits) -> Result<(), String> {
        self.bytes += len;
        if self.files.len() >= limits.files || self.bytes > limits.bytes {
            return Err(format!("files>{} or bytes>{}MB", limits.files, limits.bytes / 1024 / 1024));
        }
        self.files.push(path);
        Ok(())
    }

    fn hash(self, root: &Path, scope: Scope) -> Snapshot {
        let files = self.files.iter().filter_map(|p| hash_file(p).ok().map(|h| (rel(root, p), h))).collect();
        Snapshot { files, bytes: self.bytes, scope }
    }
}

fn walk_gitignore(root: &Path, limits: Limits) -> Result<Snapshot, String> {
    let mut found = Found::default();
    let mut walk = ignore::WalkBuilder::new(root);
    // Invariant: standard_filters(false) disables parents(), so rules outside the workspace or copy never apply.
    walk.standard_filters(false).git_ignore(true).require_git(false);
    walk.filter_entry(|e| {
        let name = e.file_name().to_string_lossy();
        e.depth() == 0 || !(is_git_meta(&name) || (e.file_type().is_some_and(|t| t.is_dir()) && skip_dir(&name)))
    });
    for entry in walk.build() {
        let entry = entry.map_err(|e| format!("{}: {e}", root.display()))?;
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        found.add(entry.into_path(), meta.len(), limits)?;
    }
    Ok(found.hash(root, Scope::GitIgnore))
}

fn walk_all(root: &Path, limits: Limits) -> Result<Snapshot, String> {
    let mut found = Found::default();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let rd = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(e) if dir == root => return Err(format!("{}: {e}", root.display())),
            Err(_) => continue,
        };
        for entry in rd.flatten() {
            let Ok(ft) = entry.file_type() else { continue };
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            // 심볼릭 링크는 따라가지 않는다(폴더 밖으로 나가거나 순환할 수 있다).
            if ft.is_symlink() || is_git_meta(&name) {
                continue;
            }
            if ft.is_dir() {
                if !skip_dir(&name) {
                    stack.push(path);
                }
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            found.add(path, meta.len(), limits)?;
        }
    }
    Ok(found.hash(root, Scope::All))
}

fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root).unwrap_or(p).to_string_lossy().replace('\\', "/")
}

fn join(root: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(root.to_path_buf(), |p, part| p.join(part))
}

/// 스냅숏에 든 파일만 dst로 복사한다.
pub fn copy_tree(src: &Path, dst: &Path, snap: &Snapshot) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| format!("{}: {e}", dst.display()))?;
    for r in snap.files.keys() {
        let to = join(dst, r);
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        std::fs::copy(join(src, r), &to).map_err(|e| format!("{r}: {e}"))?;
    }
    Ok(())
}

fn git_in(dir: &Path) -> std::process::Command {
    let mut c = std::process::Command::new("git");
    c.arg("-C").arg(dir).args(["-c", "core.longpaths=true"]);
    c.stdin(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    c
}

fn git_output(c: &mut std::process::Command) -> Result<String, String> {
    let o = c.output().map_err(|e| format!("git: {e}"))?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&o.stderr).trim().to_string())
    }
}

/// worktree를 작업 폴더의 지금 상태에 맞춘다. HEAD에는 없는 커밋 전 변경(고침·지움·새 파일)을 옮기는 일이다.
fn sync_to_base(root: &Path, dir: &Path, base: &Snapshot) -> Result<(), String> {
    // 두 번째 차례는 .gitignore가 커밋 전에 바뀌었을 때만 돈다(첫 차례는 HEAD의 규칙으로 읽었다).
    for _ in 0..2 {
        let c = diff(base, &snapshot_like(dir, base.scope)?);
        for p in c.written.keys() {
            apply_one(root, dir, p, !base.files.contains_key(p))?;
        }
        for p in &c.deleted {
            apply_one(root, dir, p, false)?;
        }
        if !c.paths().iter().any(|p| p.ends_with(".gitignore")) {
            break;
        }
    }
    Ok(())
}

/// 폴더마다 같은 일을 동시에 한다(폴더끼리 겹치지 않는 일만). 하나라도 실패하면 첫 오류를 돌려준다.
fn each_dir(dirs: &[PathBuf], f: impl Fn(&Path) -> Result<(), String> + Sync) -> Result<(), String> {
    std::thread::scope(|s| {
        let jobs: Vec<_> = dirs.iter().map(|d| s.spawn(|| f(d))).collect();
        jobs.into_iter().map(|j| j.join().unwrap_or_else(|_| Err("작업 폴더 복사 실패".into()))).collect()
    })
}

fn make_worktrees(root: &Path, tmp: &Path, n: usize, base: &Snapshot) -> Result<Vec<PathBuf>, String> {
    let prefix = git_output(git_in(root).args(["rev-parse", "--show-prefix"]))?;
    // 등록은 저장소를 잠그므로 하나씩 하고, 커밋 전 변경을 옮기는 일(파일 해시)은 동시에 한다.
    let dirs = (0..n)
        .map(|i| {
            let top = tmp.join(format!("agent-{}", i + 1));
            git_output(git_in(root).args(["worktree", "add", "--detach"]).arg(&top).arg("HEAD"))?;
            // 작업 폴더가 저장소의 하위 폴더면 에이전트도 같은 하위 폴더에서 일한다.
            let dir = prefix.split('/').filter(|s| !s.is_empty()).fold(top, |p, part| p.join(part));
            std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            Ok(dir)
        })
        .collect::<Result<Vec<_>, String>>()?;
    each_dir(&dirs, |d| sync_to_base(root, d, base))?;
    Ok(dirs)
}

/// 에이전트 작업 공간을 만든다: 전부 복사할 수 있으면 사본, 너무 크면 git worktree, git을 못 쓰면 무시된 파일을 뺀 사본.
pub fn prepare_dirs(root: &Path, tmp: &Path, n: usize, base: &Snapshot) -> Result<AgentDirs, String> {
    if base.scope == Scope::GitIgnore {
        match make_worktrees(root, tmp, n, base) {
            Ok(dirs) => return Ok(AgentDirs { dirs, how: Prepared::Worktree }),
            Err(_) => remove_dirs(root, tmp),
        }
    }
    let dirs: Vec<PathBuf> = (0..n).map(|i| tmp.join(format!("agent-{}", i + 1))).collect();
    each_dir(&dirs, |d| copy_tree(root, d, base))?;
    Ok(AgentDirs { dirs, how: Prepared::Copy })
}

/// 단계 중에 늘린 워커의 작업 공간 하나(tmp 바로 아래 name). base는 지금 메인 폴더의 상태다.
pub fn prepare_extra(root: &Path, tmp: &Path, name: &str, base: &Snapshot, worktree: bool) -> Result<PathBuf, String> {
    let top = tmp.join(name);
    if worktree {
        let prefix = git_output(git_in(root).args(["rev-parse", "--show-prefix"]))?;
        git_output(git_in(root).args(["worktree", "add", "--detach"]).arg(&top).arg("HEAD"))?;
        let dir = prefix.split('/').filter(|s| !s.is_empty()).fold(top, |p, part| p.join(part));
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        sync_to_base(root, &dir, base)?;
        return Ok(dir);
    }
    copy_tree(root, &top, base)?;
    Ok(top)
}

/// 작업 공간을 지운다. worktree는 저장소의 등록까지 지운다.
pub fn remove_dirs(root: &Path, tmp: &Path) {
    let worktrees: Vec<PathBuf> = std::fs::read_dir(tmp).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.join(".git").is_file()).collect();
    for w in &worktrees {
        let _ = git_output(git_in(root).args(["worktree", "remove", "--force"]).arg(w));
    }
    let _ = std::fs::remove_dir_all(tmp);
    if !worktrees.is_empty() {
        let _ = git_output(git_in(root).args(["worktree", "prune"]));
    }
}

/// 한 에이전트가 사본에서 바꾼 것
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Changes {
    /// 새로 만들었거나 고친 파일 → 새 해시
    #[serde(skip)]
    pub written: BTreeMap<String, [u8; 32]>,
    pub deleted: BTreeSet<String>,
}

impl Changes {
    pub fn paths(&self) -> Vec<String> {
        self.written.keys().chain(self.deleted.iter()).cloned().collect()
    }
}

pub fn diff(base: &Snapshot, after: &Snapshot) -> Changes {
    let mut c = Changes::default();
    for (p, h) in &after.files {
        if base.files.get(p) != Some(h) {
            c.written.insert(p.clone(), *h);
        }
    }
    for p in base.files.keys() {
        if !after.files.contains_key(p) {
            c.deleted.insert(p.clone());
        }
    }
    c
}

/// 에이전트 사본(from)의 파일 하나를 원래 폴더(to)에 쓰거나 지운다.
pub fn apply_one(from: &Path, to: &Path, rel_path: &str, delete: bool) -> Result<(), String> {
    let dst = join(to, rel_path);
    if delete {
        return match std::fs::remove_file(&dst) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("{rel_path}: {e}")),
        };
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    std::fs::copy(join(from, rel_path), &dst).map(|_| ()).map_err(|e| format!("{rel_path}: {e}"))
}

/// 워커 판을 메인 폴더에 반영한 결과
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Applied {
    /// 반영한 경로(쓰거나 지움)
    pub written: Vec<String>,
    /// 메인 폴더가 워커가 본 판과 달라 반영하지 않은 경로(지움 여부)
    pub clashes: Vec<(String, bool)>,
    pub errors: Vec<String>,
}

/// 워커 작업 공간(from)에서 바뀐 파일을 메인 폴더(to)에 반영한다. last는 워커가 그 파일을 마지막으로 본 판이다.
/// 메인 폴더 파일이 그 판과 같거나 이미 워커 판과 같으면 반영하고, 다른 쪽이 먼저 고쳤으면 덮지 않고 충돌로 돌려준다.
pub fn apply_checked(to: &Path, from: &Path, last: &Snapshot, changes: &Changes) -> Applied {
    let mut out = Applied::default();
    let now = |p: &str| hash_file(&join(to, p)).ok();
    for (p, h) in &changes.written {
        let main = now(p);
        if main.as_ref() == Some(h) {
            continue;
        }
        if main.as_ref() == last.files.get(p) {
            match apply_one(from, to, p, false) {
                Ok(()) => out.written.push(p.clone()),
                Err(e) => out.errors.push(e),
            }
        } else {
            out.clashes.push((p.clone(), false));
        }
    }
    for p in &changes.deleted {
        let main = now(p);
        if main.is_none() {
            continue;
        }
        if main.as_ref() == last.files.get(p) {
            match apply_one(from, to, p, true) {
                Ok(()) => out.written.push(p.clone()),
                Err(e) => out.errors.push(e),
            }
        } else {
            out.clashes.push((p.clone(), true));
        }
    }
    out
}

/// 충돌 파일 한 판을 통합 AI에게 보여 줄 글(너무 크면 None → 경로만 준다)
pub fn read_version(dir: &Path, rel_path: &str) -> Option<String> {
    let t = std::fs::read_to_string(join(dir, rel_path)).ok()?;
    (t.chars().count() <= CONFLICT_CHARS).then_some(t)
}

pub fn version_path(dir: &Path, rel_path: &str) -> String {
    join(dir, rel_path).to_string_lossy().to_string()
}

/// 나누는 AI에게 보여 줄 파일 목록(앞쪽 몇 줄)
pub fn listing(snap: &Snapshot) -> String {
    let mut s: String = snap.files.keys().take(LISTING_LINES).map(|p| format!("{p}\n")).collect();
    if snap.files.len() > LISTING_LINES {
        s.push_str(&format!("… (+{})\n", snap.files.len() - LISTING_LINES));
    }
    if s.is_empty() {
        s.push_str("(empty)\n");
    }
    s
}

/// 하위 작업 하나
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Part {
    pub title: String,
    pub task: String,
    /// 이 에이전트가 맡는 파일·폴더(다른 에이전트는 건드리지 않는다)
    #[serde(default)]
    pub files: Vec<String>,
    /// 이 하위 작업을 함께 맡는 워커 수. 2 이상이면 그룹으로 대화하며 같은 작업 공간에서 일한다.
    #[serde(default = "one")]
    pub members: usize,
    /// 이 하위 작업(팀)이 속한 부서들(바깥 → 안쪽). 비어 있으면 총괄 바로 아래다.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub org: Vec<String>,
}

fn one() -> usize {
    1
}

/// 나누는 AI의 답에서 하위 작업 목록(JSON 배열)을 꺼낸다. 최대 n개, 둘 미만이면 None.
pub fn parse_parts(text: &str, n: usize) -> Option<Vec<Part>> {
    let body = match text.find("```") {
        Some(i) => {
            let rest = &text[i + 3..];
            let rest = rest.strip_prefix("json").unwrap_or(rest);
            rest.split("```").next().unwrap_or(rest)
        }
        None => text,
    };
    let (a, b) = (body.find('[')?, body.rfind(']')?);
    if b <= a {
        return None;
    }
    let parts: Vec<Part> = serde_json::from_str(&body[a..=b]).ok()?;
    let mut parts: Vec<Part> = parts.into_iter().filter(|p| !p.task.trim().is_empty()).take(n).collect();
    for p in &mut parts {
        p.members = p.members.max(1);
        p.org = p.org.iter().map(|d| d.trim().to_string()).filter(|d| !d.is_empty()).take(MAX_ORG_DEPTH).collect();
    }
    // 합이 워커 수를 넘으면 가장 큰 그룹부터 줄인다.
    while parts.iter().map(|p| p.members).sum::<usize>() > n {
        if let Some(big) = parts.iter_mut().filter(|p| p.members > 1).max_by_key(|p| p.members) {
            big.members -= 1;
        } else {
            break;
        }
    }
    (parts.iter().map(|p| p.members).sum::<usize>() >= 2).then_some(parts)
}

/// 나누는 AI에게 적힌 팀 구성을 그대로 지키라고 알린다.
pub fn teams_note(sizes: &[usize], depts: &[Vec<String>]) -> String {
    let list = sizes.iter().map(usize::to_string).collect::<Vec<_>>().join(", ");
    let mut s = format!("반드시 작성된 팀 순서대로 정확히 {count}개의 하위 작업을 생성하고, 각 members 값은 {list}의 순서대로 지정하십시오. 팀을 병합하거나 추가해서는 안 됩니다.", count = sizes.len());
    if depts.iter().any(|d| !d.is_empty()) {
        let orgs = depts.iter().enumerate().map(|(i, d)| format!("{}팀: {}", i + 1, if d.is_empty() { "-".to_string() } else { d.join(" / ") })).collect::<Vec<_>>().join(", ");
        s.push_str(&format!(" 팀마다 부서는 정해져 있습니다({orgs}). 같은 부서의 팀끼리 얽힌 일을 맡기세요."));
    }
    s
}

/// 하위 작업이 들어갈 수 있는 부서의 최대 깊이
pub const MAX_ORG_DEPTH: usize = 4;

/// 요청문에 적힌 팀 크기대로 맞추고, 적힌 부서가 있으면 그대로 쓴다.
pub fn fit_to_org(parts: Vec<Part>, sizes: &[usize], depts: &[Vec<String>]) -> Vec<Part> {
    let mut parts = fit_to_teams(parts, sizes);
    if depts.iter().any(|d| !d.is_empty()) {
        for (p, d) in parts.iter_mut().zip(depts) {
            p.org = d.clone();
        }
    }
    parts
}

/// 요청문에 적힌 팀 크기대로 맞춘다. 팀보다 많은 하위 작업은 마지막 팀이 이어 맡는다.
pub fn fit_to_teams(mut parts: Vec<Part>, sizes: &[usize]) -> Vec<Part> {
    if parts.len() > sizes.len() && !sizes.is_empty() {
        let extra: Vec<Part> = parts.drain(sizes.len()..).collect();
        if let Some(last) = parts.last_mut() {
            for e in extra {
                last.task = format!("{}\n\n{}: {}", last.task, e.title, e.task);
                last.files.extend(e.files);
            }
        }
    }
    for (p, n) in parts.iter_mut().zip(sizes) {
        p.members = *n;
    }
    parts
}

/// 하위 작업으로 나누라는 요청(나누는 AI는 대화 모드로 돈다)
pub fn split_prompt(request: &str, listing: &str, n: usize) -> String {
    format!(
        "[요청]\n{request}\n\n[작업 폴더의 파일 목록]\n{listing}\n\
         이 요청을 AI 에이전트 최대 {n}개가 동시에 나눠 맡을 수 있게 서로 겹치지 않는 하위 작업으로 나누세요.\n\
         - 각 에이전트는 작업 폴더의 따로 된 사본에서 일하고, 끝나면 바뀐 파일만 합칩니다. 그러니 두 하위 작업이 같은 파일을 고치지 않게 나누세요.\n\
         - 공통 설정·진입 파일(package.json, main 등)처럼 여럿이 고쳐야 하는 파일은 한 하위 작업에만 맡기고, 다른 작업의 task에는 '그 파일은 건드리지 말 것'이라고 적으세요.\n\
         - 각 task는 다른 에이전트의 결과를 보지 않고도 끝낼 수 있게 필요한 이름·경로·인터페이스를 구체적으로 적으세요.\n\
         - 나눌수록 손해인 작은 요청이거나 서로 너무 얽혀 있으면 하위 작업 1개만 쓰세요.\n\
         - 그룹: members가 2 이상인 하위 작업은 그 수만큼의 워커가 한 그룹이 되어 대화하며 같은 작업 공간에서 함께 합니다. 요청문에 적힌 그룹은 그대로 따르세요. 적혀 있지 않으면 서로 많이 얽혀 혼자 하기 어려운 하위 작업만 묶고 나머지는 members를 1로 두세요. members의 합은 {n}을 넘을 수 없습니다.\n\
         - 부서(org): 요청문이 팀을 본부·부서로 묶었거나 하위 작업이 6개 이상인 큰 작업이면, 서로 맞춰야 하는 하위 작업끼리 같은 부서 이름을 org에 쓰세요(바깥 부서부터, 최대 {depth}단계). 같은 부서의 팀장끼리만 짧게 보고를 주고받으므로, 서로 상관없는 하위 작업은 다른 부서에 두거나 org를 비우세요.\n\
         파일을 고치지 말고, 설명 없이 아래 형식의 JSON 배열 하나만 답하세요.\n\
         [{{\"title\": \"짧은 이름\", \"task\": \"이 에이전트가 할 일(구체적으로)\", \"files\": [\"맡는 파일·폴더 경로\"], \"members\": 1, \"org\": []}}]",
        depth = MAX_ORG_DEPTH
    )
}

/// 하위 작업 하나를 맡은 에이전트에게 붙이는 지시
pub fn part_system(i: usize, parts: &[Part]) -> String {
    let me = &parts[i];
    let others: String = parts
        .iter()
        .enumerate()
        .filter(|(j, _)| *j != i)
        .map(|(j, p)| format!("- 에이전트 {}: {} ({})\n", j + 1, p.title, if p.files.is_empty() { "-".to_string() } else { p.files.join(", ") }))
        .collect();
    format!(
        "\n[병렬 작업] 이 요청은 에이전트 {total}개가 나눠 동시에 작업합니다. 당신은 에이전트 {num} '{title}'입니다.\n\
         당신은 작업 폴더의 사본에서 일합니다. 끝나면 당신이 바꾼 파일만 원래 폴더로 합쳐지고, 통합 AI가 전체를 맞춰 봅니다.\n\
         - 맡은 범위만 고치세요. 다른 에이전트가 맡은 파일은 만들거나 고치지 마세요.\n\
         다른 에이전트:\n{others}\
         - 의존성 설치·빌드는 해도 되지만 node_modules·target 같은 산출물은 합치지 않습니다.\n\
         - 끝나면 바꾼 파일과 다른 에이전트·통합 AI가 알아야 할 것(내보낸 함수·경로·가정)을 짧게 요약하세요.\n",
        total = parts.len(),
        num = i + 1,
        title = me.title,
    )
}

pub fn part_prompt(request: &str, part: &Part) -> String {
    let files = if part.files.is_empty() { String::new() } else { format!("\n[맡은 파일·폴더]\n{}\n", part.files.join("\n")) };
    format!("[전체 요청]\n{request}\n\n[당신이 맡은 하위 작업: {}]\n{}\n{files}", part.title, part.task)
}

/// 부서가 하나라도 있으면 조직으로 일한다(팀장끼리 보고를 주고받는다).
pub fn org_active(paths: &[Vec<String>]) -> bool {
    paths.iter().any(|p| !p.is_empty())
}

/// 부서를 대표하는 하위 작업(부서장): 부서 안 첫 하위 작업
fn head_of(paths: &[Vec<String>], dept: &[String]) -> Option<usize> {
    paths.iter().position(|p| p.starts_with(dept))
}

/// 하위 작업 i의 팀장 보고를 받는 하위 작업들. 한 부서(또는 총괄 바로 아래) 안에서 바로 아래 단위의 대표끼리만 주고받는다.
/// 바로 아래 단위는 팀(하위 작업) 또는 하위 부서이고, 하위 부서는 그 부서의 첫 팀장(부서장)이 대표한다.
pub fn org_peers(paths: &[Vec<String>]) -> Vec<Vec<usize>> {
    if !org_active(paths) {
        return vec![vec![]; paths.len()];
    }
    // 부서 경로 → 그 안에서 말하는 대표들
    let mut rooms: BTreeMap<Vec<String>, BTreeSet<usize>> = BTreeMap::new();
    for (i, p) in paths.iter().enumerate() {
        rooms.entry(p.clone()).or_default().insert(i);
        // 부서장이면 한 단계 위 방에서도 그 부서를 대표한다.
        for level in (0..p.len()).rev() {
            if head_of(paths, &p[..=level]) != Some(i) {
                break;
            }
            rooms.entry(p[..level].to_vec()).or_default().insert(i);
        }
    }
    let mut out = vec![BTreeSet::new(); paths.len()];
    for who in rooms.values() {
        for i in who {
            out[*i].extend(who.iter().filter(|j| *j != i).copied());
        }
    }
    out.into_iter().map(|s| s.into_iter().collect()).collect()
}

/// 조직으로 일할 때 하위 작업 i의 워커에게 붙이는 안내(짧게)
pub fn org_note(i: usize, parts: &[Part], peers: &[Vec<usize>], group: bool) -> String {
    let Some(me) = parts.get(i) else { return String::new() };
    let dept = if me.org.is_empty() { "총괄 직속".to_string() } else { me.org.join(" / ") };
    let names: Vec<String> = peers.get(i).into_iter().flatten().filter_map(|j| parts.get(*j)).map(|p| format!("'{}'", p.title)).collect();
    let mut s = format!("\n[조직] 당신의 팀 '{}'은(는) {dept} 소속입니다.", me.title);
    if names.is_empty() {
        s.push_str(" 다른 팀과 직접 대화하지 않습니다.\n");
    } else if group {
        s.push_str(&format!(" 그룹장(그룹원 1)이 팀을 대표해 {} 팀장과 짧은 보고를 주고받습니다.\n", names.join(", ")));
    } else {
        s.push_str(&format!(" 당신이 팀장으로 {} 팀장과 짧은 보고를 주고받습니다. 받은 보고는 다음 차례 앞에 붙습니다.\n", names.join(", ")));
    }
    s
}

/// 통합 AI가 받는 각 에이전트의 결과
pub struct PartResult<'a> {
    pub title: &'a str,
    pub who: String,
    pub ok: bool,
    pub summary: &'a str,
    pub files: Vec<String>,
}

/// 충돌 파일의 다른 판(먼저 쓴 판은 이미 원래 폴더에 있다)
pub struct ConflictView {
    pub path: String,
    pub kept: String,
    pub others: Vec<(String, String, Option<String>)>,
}

pub const INTEGRATE_SYSTEM: &str = "\n[통합] 여러 에이전트가 나눠서 한 작업이 이미 이 작업 폴더에 합쳐졌습니다. 당신은 통합 AI입니다. 결과가 하나의 완성된 결과물로 맞물리게 마무리하세요. 파일 수정은 이미 허락받았습니다.\n";

pub fn integrate_prompt(request: &str, results: &[PartResult], conflicts: &[ConflictView]) -> String {
    let mut s = format!("[전체 요청]\n{request}\n\n");
    for (i, r) in results.iter().enumerate() {
        let files = if r.files.is_empty() { "-".to_string() } else { r.files.join(", ") };
        if r.ok {
            s.push_str(&format!("[에이전트 {}: {} · {}] 합친 파일: {files}\n{}\n\n", i + 1, r.title, r.who, r.summary.chars().take(3000).collect::<String>()));
        } else {
            s.push_str(&format!("[에이전트 {}: {} · {}] 실패하거나 중지되어 합치지 않았습니다. 이 부분은 당신이 직접 끝내세요.\n\n", i + 1, r.title, r.who));
        }
    }
    if !conflicts.is_empty() {
        s.push_str("[충돌] 아래 파일은 여러 에이전트가 서로 다르게 고쳤습니다. 작업 폴더에는 첫 판이 들어 있습니다. 다른 판의 변경을 빠짐없이 합쳐 하나로 고치세요.\n");
        for c in conflicts {
            s.push_str(&format!("\n### {} (작업 폴더에 있는 판: {})\n", c.path, c.kept));
            for (who, path, text) in &c.others {
                match text {
                    Some(t) => s.push_str(&format!("--- {who}의 판 ---\n{t}\n")),
                    None => s.push_str(&format!("--- {who}의 판: {path} (읽어서 합치세요) ---\n")),
                }
            }
        }
        s.push('\n');
    }
    s.push_str(
        "할 일:\n\
         1. 각 부분이 서로 맞물리는지(가져오기·경로·이름·설정) 확인하고 어긋난 곳을 고치세요. 충돌 파일이 있으면 하나로 합치세요.\n\
         2. 실패한 부분이 있으면 직접 끝내세요.\n\
         3. 명령을 쓸 수 있으면 설치·빌드·테스트를 돌려 실제로 동작하는지 확인하고 오류를 고치세요.\n\
         4. 끝나면 사용자에게 줄 하나의 최종 결과 요약을 쓰세요: 무엇을 만들었고·바꿨는지, 실행 방법, 가정. '에이전트 1은' 같은 과정 서술로 시작하지 말고 결과부터 쓰세요.",
    );
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_copy_diff_and_apply_round_trip() {
        let src = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(src.path().join("a/b")).unwrap();
        std::fs::create_dir_all(src.path().join("node_modules/x")).unwrap();
        std::fs::write(src.path().join("a/b/one.txt"), "1").unwrap();
        std::fs::write(src.path().join("two.txt"), "2").unwrap();
        std::fs::write(src.path().join("node_modules/x/big.js"), "skip").unwrap();
        let base = snapshot(src.path()).unwrap();
        assert_eq!(base.files.keys().cloned().collect::<Vec<_>>(), vec!["a/b/one.txt", "two.txt"], "의존성 폴더는 뺀다");

        let copy = tempfile::tempdir().unwrap();
        copy_tree(src.path(), copy.path(), &base).unwrap();
        std::fs::write(copy.path().join("a/b/one.txt"), "changed").unwrap();
        std::fs::write(copy.path().join("new.txt"), "n").unwrap();
        std::fs::remove_file(copy.path().join("two.txt")).unwrap();
        std::fs::create_dir_all(copy.path().join("node_modules/y")).unwrap();
        std::fs::write(copy.path().join("node_modules/y/z.js"), "z").unwrap();
        let c = diff(&base, &snapshot(copy.path()).unwrap());
        assert_eq!(c.written.keys().cloned().collect::<Vec<_>>(), vec!["a/b/one.txt", "new.txt"]);
        assert_eq!(c.deleted.iter().cloned().collect::<Vec<_>>(), vec!["two.txt"]);

        let a = apply_checked(src.path(), copy.path(), &base, &c);
        assert!(a.clashes.is_empty() && a.errors.is_empty());
        assert_eq!(std::fs::read_to_string(src.path().join("a/b/one.txt")).unwrap(), "changed");
        assert!(src.path().join("new.txt").exists() && !src.path().join("two.txt").exists());
    }

    #[test]
    fn ignored_files_are_left_out_when_scope_is_gitignore() {
        let src = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join(".gitignore"), ".plexus/
run/
*.log
").unwrap();
        std::fs::create_dir_all(src.path().join(".plexus/cache")).unwrap();
        std::fs::write(src.path().join(".plexus/cache/huge.bin"), b"x").unwrap();
        std::fs::create_dir_all(src.path().join("src")).unwrap();
        std::fs::write(src.path().join("src/a.txt"), b"a").unwrap();
        std::fs::write(src.path().join("a.log"), b"l").unwrap();
        let base = snapshot_like(src.path(), Scope::GitIgnore).unwrap();
        assert_eq!(base.files.keys().map(String::as_str).collect::<Vec<_>>(), vec![".gitignore", "src/a.txt"]);

        // 사본에는 .git이 없어도 같은 규칙으로 읽어 게임 실행 폴더 같은 산출물을 합치지 않는다.
        let copy = tempfile::tempdir().unwrap();
        copy_tree(src.path(), copy.path(), &base).unwrap();
        std::fs::create_dir_all(copy.path().join("run")).unwrap();
        std::fs::write(copy.path().join("run/world.dat"), b"w").unwrap();
        std::fs::write(copy.path().join("src/a.txt"), b"changed").unwrap();
        let after = snapshot_like(copy.path(), base.scope).unwrap();
        assert_eq!(diff(&base, &after).paths(), vec!["src/a.txt".to_string()]);
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let o = std::process::Command::new("git").arg("-C").arg(dir).args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "core.autocrlf=false"]).args(args).output().unwrap();
        assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).to_string()
    }

    fn repo() -> tempfile::TempDir {
        let r = tempfile::tempdir().unwrap();
        git(r.path(), &["init", "-q"]);
        std::fs::write(r.path().join(".gitignore"), "cache/\n").unwrap();
        std::fs::create_dir_all(r.path().join("sub")).unwrap();
        std::fs::write(r.path().join("sub/kept.txt"), b"committed").unwrap();
        std::fs::write(r.path().join("sub/edited.txt"), b"committed").unwrap();
        std::fs::write(r.path().join("sub/removed.txt"), b"committed").unwrap();
        git(r.path(), &["add", "."]);
        git(r.path(), &["commit", "-qm", "base"]);
        // 커밋하지 않은 상태: 고침·지움·새 파일·무시된 파일
        std::fs::write(r.path().join("sub/edited.txt"), b"uncommitted").unwrap();
        std::fs::remove_file(r.path().join("sub/removed.txt")).unwrap();
        std::fs::write(r.path().join("sub/untracked.txt"), b"new").unwrap();
        std::fs::create_dir_all(r.path().join("cache")).unwrap();
        std::fs::write(r.path().join("cache/big.bin"), b"ignored").unwrap();
        r
    }

    #[test]
    fn worktrees_start_from_uncommitted_state_and_never_merge_git_metadata() {
        let r = repo();
        let tmp = tempfile::tempdir().unwrap();
        let base = snapshot_like(r.path(), Scope::GitIgnore).unwrap();
        let prepared = prepare_dirs(r.path(), tmp.path(), 2, &base).unwrap();
        assert_eq!(prepared.how, Prepared::Worktree);
        assert_eq!(prepared.dirs.len(), 2);
        for d in &prepared.dirs {
            assert!(d.join(".git").is_file(), "진짜 worktree");
            assert_eq!(std::fs::read(d.join("sub/edited.txt")).unwrap(), b"uncommitted");
            assert_eq!(std::fs::read(d.join("sub/untracked.txt")).unwrap(), b"new");
            assert!(!d.join("sub/removed.txt").exists());
            assert!(!d.join("cache").exists());
            assert!(diff(&base, &snapshot_like(d, base.scope).unwrap()).paths().is_empty(), "시작할 때 바뀐 것이 없어야 한다");
        }
        std::fs::write(prepared.dirs[0].join("sub/kept.txt"), b"agent").unwrap();
        assert_eq!(diff(&base, &snapshot_like(&prepared.dirs[0], base.scope).unwrap()).paths(), vec!["sub/kept.txt".to_string()]);
        assert_eq!(git(r.path(), &["status", "--short", "--", "sub/kept.txt"]), "", "원래 폴더는 그대로");

        remove_dirs(r.path(), tmp.path());
        assert!(!tmp.path().exists());
        assert_eq!(git(r.path(), &["worktree", "list"]).lines().count(), 1, "등록도 지운다");
    }

    #[test]
    fn extra_workspace_joins_the_stage_and_is_cleaned_up() {
        let r = repo();
        let tmp = tempfile::tempdir().unwrap();
        let base = snapshot_like(r.path(), Scope::GitIgnore).unwrap();
        let prepared = prepare_dirs(r.path(), tmp.path(), 1, &base).unwrap();
        assert!(tmp.path().join("agent-1").join(".git").is_file());
        // 단계 중에 메인 폴더가 바뀐 뒤 늘린 워커는 바뀐 상태에서 시작한다.
        std::fs::write(r.path().join("sub/kept.txt"), b"merged").unwrap();
        let now = snapshot_like(r.path(), Scope::GitIgnore).unwrap();
        let extra = prepare_extra(r.path(), tmp.path(), "agent-x1", &now, true).unwrap();
        assert!(extra.join(".git").is_file());
        assert_eq!(std::fs::read(extra.join("sub/kept.txt")).unwrap(), b"merged");
        assert!(diff(&now, &snapshot_like(&extra, now.scope).unwrap()).paths().is_empty());
        let copied = prepare_extra(r.path(), tmp.path(), "agent-x2", &now, false).unwrap();
        assert_eq!(std::fs::read(copied.join("sub/edited.txt")).unwrap(), b"uncommitted");
        drop(prepared);
        remove_dirs(r.path(), tmp.path());
        assert_eq!(git(r.path(), &["worktree", "list"]).lines().count(), 1, "늘린 공간의 등록도 지운다");
    }

    #[test]
    fn worktree_agents_work_in_the_same_subfolder() {
        let r = repo();
        let tmp = tempfile::tempdir().unwrap();
        let root = r.path().join("sub");
        let base = snapshot_like(&root, Scope::GitIgnore).unwrap();
        let prepared = prepare_dirs(&root, tmp.path(), 1, &base).unwrap();
        assert_eq!(prepared.how, Prepared::Worktree);
        assert_eq!(std::fs::read(prepared.dirs[0].join("edited.txt")).unwrap(), b"uncommitted");
        assert!(diff(&base, &snapshot_like(&prepared.dirs[0], base.scope).unwrap()).paths().is_empty());
        remove_dirs(&root, tmp.path());
        assert_eq!(git(r.path(), &["worktree", "list"]).lines().count(), 1);
    }

    #[test]
    fn folder_without_a_commit_falls_back_to_filtered_copy() {
        let plain = tempfile::tempdir().unwrap();
        std::fs::write(plain.path().join(".gitignore"), "cache/\n").unwrap();
        std::fs::write(plain.path().join("a.txt"), b"a").unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let base = snapshot_like(plain.path(), Scope::GitIgnore).unwrap();
        let prepared = prepare_dirs(plain.path(), tmp.path(), 1, &base).unwrap();
        assert_eq!(prepared.how, Prepared::Copy);
        assert_eq!(std::fs::read(prepared.dirs[0].join("a.txt")).unwrap(), b"a");
        // 작은 폴더는 지금처럼 전부 복사한다.
        let r = repo();
        let all = snapshot_like(r.path(), Scope::All).unwrap();
        let t2 = tempfile::tempdir().unwrap();
        let p2 = prepare_dirs(r.path(), t2.path(), 1, &all).unwrap();
        assert_eq!(p2.how, Prepared::Copy);
        assert!(p2.dirs[0].join("cache/big.bin").exists() && !p2.dirs[0].join(".git").exists());
    }

    #[test]
    fn oversized_folder_falls_back_to_gitignore_scope() {
        let src = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join(".gitignore"), "assets/
").unwrap();
        std::fs::write(src.path().join("a.txt"), b"a").unwrap();
        std::fs::create_dir_all(src.path().join("assets")).unwrap();
        std::fs::write(src.path().join("assets/big.bin"), b"0123456789").unwrap();
        let small = Limits { files: 100, bytes: 12 };
        let snap = snapshot_within(src.path(), small).unwrap();
        assert_eq!(snap.scope, Scope::GitIgnore);
        assert!(!snap.files.contains_key("assets/big.bin"));
        let roomy = Limits { files: 100, bytes: 1000 };
        assert_eq!(snapshot_within(src.path(), roomy).unwrap().scope, Scope::All, "한도 안이면 지금처럼 전부 복사");
        assert!(snapshot_within(src.path(), Limits { files: 1, bytes: 1 }).is_err(), "걸러도 크면 실패");
    }

    #[test]
    fn checked_apply_never_overwrites_a_file_someone_else_changed_first() {
        let main = tempfile::tempdir().unwrap();
        std::fs::write(main.path().join("shared.txt"), "base").unwrap();
        std::fs::write(main.path().join("mine.txt"), "base").unwrap();
        std::fs::write(main.path().join("gone.txt"), "base").unwrap();
        let base = snapshot(main.path()).unwrap();
        let w = tempfile::tempdir().unwrap();
        copy_tree(main.path(), w.path(), &base).unwrap();
        // 워커: 세 파일을 바꾸고 하나는 지운다
        std::fs::write(w.path().join("shared.txt"), "worker").unwrap();
        std::fs::write(w.path().join("mine.txt"), "worker").unwrap();
        std::fs::write(w.path().join("new.txt"), "n").unwrap();
        std::fs::remove_file(w.path().join("gone.txt")).unwrap();
        // 그 사이 다른 워커가 shared.txt를 먼저 반영했다
        std::fs::write(main.path().join("shared.txt"), "other").unwrap();
        let after = snapshot(w.path()).unwrap();
        let a = apply_checked(main.path(), w.path(), &base, &diff(&base, &after));
        assert_eq!(a.clashes, vec![("shared.txt".to_string(), false)]);
        assert_eq!(std::fs::read_to_string(main.path().join("shared.txt")).unwrap(), "other", "먼저 반영된 판을 덮지 않는다");
        assert_eq!(std::fs::read_to_string(main.path().join("mine.txt")).unwrap(), "worker");
        assert!(main.path().join("new.txt").exists() && !main.path().join("gone.txt").exists());
        assert_eq!(a.written.len(), 3);
        // 두 번째 반영은 지난 반영 이후 바뀐 것만 본다
        std::fs::write(w.path().join("mine.txt"), "worker 2").unwrap();
        let again = snapshot(w.path()).unwrap();
        let b = apply_checked(main.path(), w.path(), &after, &diff(&after, &again));
        assert_eq!((b.written.clone(), b.clashes.len()), (vec!["mine.txt".to_string()], 0));
    }

    #[test]
    fn teams_note_states_count_and_sizes() {
        let n = teams_note(&[2, 3], &[vec![], vec![]]);
        assert!(n.contains('2') && n.contains("2, 3") && n.contains("members") && !n.contains("부서"), "{n}");
        let d = teams_note(&[2, 3], &[vec!["앱 본부".into()], vec![]]);
        assert!(d.contains("1팀: 앱 본부") && d.contains("2팀: -"), "{d}");
    }

    fn paths(list: &[&[&str]]) -> Vec<Vec<String>> {
        list.iter().map(|p| p.iter().map(|x| x.to_string()).collect()).collect()
    }

    #[test]
    fn org_peers_talk_only_between_leaders_of_the_same_level() {
        assert_eq!(org_peers(&paths(&[&[], &[], &[]])), vec![Vec::<usize>::new(); 3], "부서가 없으면 지금처럼 서로 대화하지 않는다");
        // 0,1: 앱 본부 / 2,3: 앱 본부·서버 부서 / 4: 데이터 본부 / 5: 직속
        let p = paths(&[&["앱"], &["앱"], &["앱", "서버"], &["앱", "서버"], &["데이터"], &[]]);
        let peers = org_peers(&p);
        assert_eq!(peers[0], vec![1, 2, 4, 5], "앱 본부장(0)은 본부 안 대표와, 다른 본부장·직속 팀과 말한다");
        assert_eq!(peers[1], vec![0, 2], "팀장은 같은 본부 대표들과만");
        assert_eq!(peers[2], vec![0, 1, 3], "서버 부서장은 부서 안과 본부 안에서");
        assert_eq!(peers[3], vec![2], "부서 안 팀장은 부서 안에서만");
        assert_eq!(peers[4], vec![0, 5]);
        assert_eq!(peers[5], vec![0, 4]);
    }

    #[test]
    fn written_departments_are_kept_and_cleaned() {
        let parts = parse_parts("[{\"title\":\"a\",\"task\":\"x\",\"org\":[\" 앱 \",\"\"]},{\"title\":\"b\",\"task\":\"y\"}]", 4).unwrap();
        assert_eq!(parts[0].org, vec!["앱".to_string()]);
        let fit = fit_to_org(parts, &[1, 1], &[vec![], vec!["서버 본부".into()]]);
        assert_eq!((fit[0].org.len(), fit[1].org.clone()), (0, vec!["서버 본부".to_string()]), "적힌 부서가 이긴다");
        let note = org_note(1, &fit, &org_peers(&fit.iter().map(|p| p.org.clone()).collect::<Vec<_>>()), false);
        assert!(note.contains("서버 본부") && note.contains("'a'"), "{note}");
    }

    #[test]
    fn fit_to_teams_forces_written_sizes_and_keeps_every_task() {
        let many = parse_parts("[{\"title\":\"a\",\"task\":\"x\"},{\"title\":\"b\",\"task\":\"y\",\"members\":3},{\"title\":\"c\",\"task\":\"z\"}]", 8).unwrap();
        let fit = fit_to_teams(many, &[2, 2]);
        assert_eq!(fit.iter().map(|p| p.members).collect::<Vec<_>>(), vec![2, 2]);
        assert!(fit[1].task.contains('y') && fit[1].task.contains('z'), "넘친 작업은 버리지 않고 마지막 팀에 붙인다: {}", fit[1].task);
        let few = parse_parts("[{\"title\":\"a\",\"task\":\"x\"},{\"title\":\"b\",\"task\":\"y\"}]", 8).unwrap();
        assert_eq!(fit_to_teams(few, &[2, 1, 2]).iter().map(|p| p.members).collect::<Vec<_>>(), vec![2, 1]);
    }

    #[test]
    fn parts_read_members_and_cap_total() {
        let one = parse_parts("[{\"title\":\"a\",\"task\":\"x\"},{\"title\":\"b\",\"task\":\"y\",\"members\":2}]", 4).unwrap();
        assert_eq!(one.iter().map(|p| p.members).collect::<Vec<_>>(), vec![1, 2], "members 기본값은 1");
        let capped = parse_parts("[{\"title\":\"a\",\"task\":\"x\",\"members\":3},{\"title\":\"b\",\"task\":\"y\",\"members\":3}]", 4).unwrap();
        assert_eq!(capped.iter().map(|p| p.members).sum::<usize>(), 4, "합이 워커 수를 넘지 않는다");
        assert!(capped.iter().all(|p| p.members >= 1));
        let zero = parse_parts("[{\"title\":\"a\",\"task\":\"x\",\"members\":0},{\"title\":\"b\",\"task\":\"y\"}]", 4).unwrap();
        assert_eq!(zero[0].members, 1);
    }

    #[test]
    fn one_group_alone_counts_as_a_split() {
        let g = parse_parts("[{\"title\":\"a\",\"task\":\"x\",\"members\":2}]", 2).unwrap();
        assert_eq!(g.len(), 1, "워커 둘이 한 그룹이면 나눌 수 있는 것");
        assert!(parse_parts("[{\"title\":\"a\",\"task\":\"x\"}]", 2).is_none(), "워커 하나뿐이면 나눈 것이 아니다");
    }

    #[test]
    fn split_prompt_demands_written_groups() {
        let p = split_prompt("요청", "목록", 4);
        assert!(p.contains("members") && p.contains("요청문에 적힌 그룹"), "{p}");
    }

    #[test]
    fn parts_are_parsed_from_fenced_or_bare_json() {
        let t = "나눴습니다.\n```json\n[{\"title\":\"API\",\"task\":\"서버\",\"files\":[\"server/\"]},{\"title\":\"UI\",\"task\":\"화면\"}]\n```";
        let p = parse_parts(t, 5).unwrap();
        assert_eq!(p.len(), 2);
        assert_eq!(p[1].files, Vec::<String>::new());
        assert_eq!(parse_parts(t, 1), None, "둘 미만이면 나누지 않는다");
        assert_eq!(parse_parts("[{\"title\":\"하나\",\"task\":\"다\"}]", 3), None);
        assert_eq!(parse_parts("모르겠습니다", 3), None);
        let three = "[{\"title\":\"a\",\"task\":\"1\"},{\"title\":\"b\",\"task\":\"2\"},{\"title\":\"c\",\"task\":\"3\"}]";
        assert_eq!(parse_parts(three, 2).unwrap().len(), 2);
        let sys = part_system(0, &parse_parts(three, 3).unwrap());
        assert!(sys.contains("에이전트 1 'a'") && sys.contains("에이전트 2: b") && !sys.contains("에이전트 1: a"));
    }
}
