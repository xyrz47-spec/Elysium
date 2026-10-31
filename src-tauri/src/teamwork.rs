//! 병렬 작업: 작업(만들기)을 여러 에이전트에게 나눠 맡긴다.
//! 에이전트마다 작업 폴더의 사본에서 따로 일하고, 끝나면 바뀐 파일만 원래 폴더로 합친다.
//! 두 에이전트가 같은 파일을 다르게 고쳤으면(충돌) 통합 AI가 원래 폴더에서 하나로 합친다.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// 사본에 넣지 않는 폴더(의존성·빌드 산출물·VCS). 에이전트가 사본에서 설치·빌드해도 합치지 않는다.
const SKIP_DIRS: &[&str] = &[
    ".git", ".hg", ".svn", "node_modules", "target", "dist", "build", "out", ".next", ".nuxt", ".svelte-kit", ".turbo", ".cache", ".parcel-cache", ".venv", "venv", "env", "__pycache__", ".pytest_cache", ".mypy_cache", ".gradle", ".idea", ".vs", "bin", "obj", "coverage", ".elysium-team",
];
/// 이보다 크면 사본을 만들지 않고 한 에이전트로 돌아간다.
const MAX_FILES: usize = 20_000;
const MAX_BYTES: u64 = 300 * 1024 * 1024;
/// 통합 AI에게 글로 보여 주는 충돌 파일 한 판의 최대 글자 수(넘으면 경로만 준다)
const CONFLICT_CHARS: usize = 12_000;
/// 나누는 AI에게 보여 주는 파일 목록의 최대 줄 수
const LISTING_LINES: usize = 300;

/// 상대 경로(슬래시) → 내용 해시
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    pub files: BTreeMap<String, [u8; 32]>,
    pub bytes: u64,
}

fn skip_dir(name: &str) -> bool {
    SKIP_DIRS.iter().any(|d| d.eq_ignore_ascii_case(name))
}

fn hash_file(p: &Path) -> std::io::Result<[u8; 32]> {
    let data = std::fs::read(p)?;
    Ok(Sha256::digest(&data).into())
}

/// 폴더 안 파일의 해시 목록. 크기 한도를 넘으면 Err(사람이 읽는 이유).
pub fn snapshot(root: &Path) -> Result<Snapshot, String> {
    let mut snap = Snapshot::default();
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
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                if !skip_dir(&name) {
                    stack.push(path);
                }
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            snap.bytes += meta.len();
            if snap.files.len() >= MAX_FILES || snap.bytes > MAX_BYTES {
                return Err(format!("files>{MAX_FILES} or bytes>{}MB", MAX_BYTES / 1024 / 1024));
            }
            let Ok(h) = hash_file(&path) else { continue };
            snap.files.insert(rel(root, &path), h);
        }
    }
    Ok(snap)
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

/// 합치는 방법: 파일마다 어느 에이전트의 판을 원래 폴더에 쓸지, 그리고 충돌 목록.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Merge {
    /// (경로, 에이전트 번호, 지움 여부)
    pub apply: Vec<(String, usize, bool)>,
    /// 둘 이상이 서로 다르게 바꾼 파일 → 바꾼 에이전트 번호들(먼저 쓴 판은 apply에도 들어 있다)
    pub conflicts: BTreeMap<String, Vec<usize>>,
}

/// None인 칸은 실패하거나 중지된 에이전트(합치지 않는다).
pub fn plan_merge(all: &[Option<Changes>]) -> Merge {
    // 경로 → (에이전트, 결과: Some(해시)=씀, None=지움)
    let mut by_path: BTreeMap<String, Vec<(usize, Option<[u8; 32]>)>> = BTreeMap::new();
    for (i, c) in all.iter().enumerate() {
        let Some(c) = c else { continue };
        for (p, h) in &c.written {
            by_path.entry(p.clone()).or_default().push((i, Some(*h)));
        }
        for p in &c.deleted {
            by_path.entry(p.clone()).or_default().push((i, None));
        }
    }
    let mut m = Merge::default();
    for (p, v) in by_path {
        let (first, result) = v[0];
        m.apply.push((p.clone(), first, result.is_none()));
        // 모두 같은 결과면(같은 내용으로 고쳤거나 모두 지웠으면) 충돌이 아니다.
        if v.iter().any(|(_, r)| *r != result) {
            m.conflicts.insert(p, v.iter().map(|(i, _)| *i).collect());
        }
    }
    m
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
    let parts: Vec<Part> = parts.into_iter().filter(|p| !p.task.trim().is_empty()).take(n).collect();
    (parts.len() >= 2).then_some(parts)
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
         파일을 고치지 말고, 설명 없이 아래 형식의 JSON 배열 하나만 답하세요.\n\
         [{{\"title\": \"짧은 이름\", \"task\": \"이 에이전트가 할 일(구체적으로)\", \"files\": [\"맡는 파일·폴더 경로\"]}}]"
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

    fn h(b: u8) -> [u8; 32] {
        [b; 32]
    }

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

        let m = plan_merge(&[Some(c)]);
        assert!(m.conflicts.is_empty());
        for (p, _, del) in &m.apply {
            apply_one(copy.path(), src.path(), p, *del).unwrap();
        }
        assert_eq!(std::fs::read_to_string(src.path().join("a/b/one.txt")).unwrap(), "changed");
        assert!(src.path().join("new.txt").exists() && !src.path().join("two.txt").exists());
    }

    #[test]
    fn merge_finds_conflicts_and_skips_failed_agents() {
        let mut a = Changes::default();
        a.written.insert("same.txt".into(), h(1));
        a.written.insert("shared.txt".into(), h(1));
        a.written.insert("only-a.txt".into(), h(1));
        let mut b = Changes::default();
        b.written.insert("same.txt".into(), h(1));
        b.written.insert("shared.txt".into(), h(2));
        b.deleted.insert("old.txt".into());
        let mut failed = Changes::default();
        failed.written.insert("shared.txt".into(), h(9));
        let m = plan_merge(&[Some(a), None, Some(b)]);
        assert_eq!(m.conflicts.keys().cloned().collect::<Vec<_>>(), vec!["shared.txt"], "같은 내용으로 고친 파일은 충돌이 아니다");
        assert_eq!(m.conflicts["shared.txt"], vec![0, 2]);
        assert!(m.apply.contains(&("old.txt".into(), 2, true)));
        assert!(m.apply.contains(&("shared.txt".into(), 0, false)));
        assert!(!m.apply.iter().any(|(_, i, _)| *i == 1));
        drop(failed);
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
