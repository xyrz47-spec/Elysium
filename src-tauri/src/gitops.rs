//! 총괄의 커밋 게이트에 쓰는 git 도우미: 저장소 확인, 바뀐 파일 비교, 검토용 diff, 지정한 파일만 커밋.
//! 사용자가 커밋하지 않고 둔 다른 변경은 건드리지 않도록 언제나 경로를 지정해 커밋한다.

use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;

/// 검토 글에 넣는 diff의 최대 글자 수
const DIFF_CHARS: usize = 24_000;

#[derive(Debug, Clone, PartialEq)]
pub struct Repo {
    /// 저장소 최상위 폴더
    pub top: PathBuf,
    /// 작업 폴더가 저장소의 하위 폴더일 때 그 경로("sub/dir/"), 최상위면 빈 문자열
    pub prefix: String,
}

fn git(dir: &Path) -> std::process::Command {
    let mut c = std::process::Command::new("git");
    // Invariant: literal pathspecs so file names with glob characters never widen a commit.
    c.arg("-C").arg(dir).args(["-c", "core.longpaths=true", "-c", "core.quotepath=false", "--literal-pathspecs"]);
    c.stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    c
}

fn run(c: &mut std::process::Command) -> Result<String, String> {
    let o = c.output().map_err(|e| format!("git: {e}"))?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&o.stderr).trim().to_string())
    }
}

/// 경로 목록을 표준 입력(NUL 구분)으로 넘긴다. 명령줄 길이 제한에 걸리지 않는다.
fn run_with_paths(mut c: std::process::Command, paths: &[String]) -> Result<String, String> {
    c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn().map_err(|e| format!("git: {e}"))?;
    {
        let mut input = child.stdin.take().ok_or("git: stdin")?;
        for p in paths {
            input.write_all(p.as_bytes()).map_err(|e| e.to_string())?;
            input.write_all(b"\0").map_err(|e| e.to_string())?;
        }
    }
    let o = child.wait_with_output().map_err(|e| format!("git: {e}"))?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&o.stderr).trim().to_string())
    }
}

/// 경로를 인자로 받는 명령을 명령줄 길이 제한 안에서 나눠 돌리고 출력을 이어 붙인다(diff·ls-tree는 표준 입력 경로를 못 받는다).
fn run_chunked(dir: &Path, args: &[&str], paths: &[String]) -> Result<String, String> {
    let mut out = String::new();
    let mut start = 0;
    while start < paths.len() {
        let (mut end, mut len) = (start, 0usize);
        while end < paths.len() && (end == start || len + paths[end].len() < 20_000) {
            len += paths[end].len() + 3;
            end += 1;
        }
        let mut c = git(dir);
        c.args(args).arg("--").args(&paths[start..end]);
        out.push_str(&run(&mut c)?);
        start = end;
    }
    Ok(out)
}

/// 커밋할 수 있는 저장소면 Some. 커밋이 하나도 없거나 병합·리베이스 중이면 None.
pub fn open(root: &Path) -> Option<Repo> {
    let top = run(git(root).args(["rev-parse", "--show-toplevel"])).ok()?.trim().to_string();
    let prefix = run(git(root).args(["rev-parse", "--show-prefix"])).ok()?.trim().to_string();
    run(git(root).args(["rev-parse", "--verify", "-q", "HEAD"])).ok()?;
    for state in ["MERGE_HEAD", "rebase-merge", "rebase-apply", "CHERRY_PICK_HEAD", "REVERT_HEAD"] {
        let p = run(git(root).args(["rev-parse", "--git-path", state])).ok()?;
        let p = PathBuf::from(p.trim());
        let p = if p.is_absolute() { p } else { root.join(p) };
        if p.exists() {
            return None;
        }
    }
    Some(Repo { top: PathBuf::from(top), prefix })
}

/// `.elysium/`을 저장소의 info/exclude에 넣어 작업 공간이 변경으로 보이지 않게 한다(.gitignore는 건드리지 않는다).
pub fn exclude_workspace_dir(root: &Path) {
    let Ok(dir) = run(git(root).args(["rev-parse", "--git-common-dir"])) else { return };
    let dir = PathBuf::from(dir.trim());
    let dir = if dir.is_absolute() { dir } else { root.join(dir) };
    let file = dir.join("info").join("exclude");
    let now = std::fs::read_to_string(&file).unwrap_or_default();
    if now.lines().any(|l| l.trim() == ".elysium/" || l.trim() == "/.elysium/") {
        return;
    }
    let _ = std::fs::create_dir_all(dir.join("info"));
    let sep = if now.is_empty() || now.ends_with('\n') { "" } else { "\n" };
    let _ = std::fs::write(&file, format!("{now}{sep}.elysium/\n"));
}

impl Repo {
    /// 최근 커밋 제목 몇 줄(총괄이 메시지 형식을 맞추게 한다)
    pub fn recent_log(&self, n: usize) -> String {
        run(git(&self.top).args(["log", "--no-color", "--format=%s", "-n", &n.to_string()])).unwrap_or_default()
    }

    /// 지금 커밋되지 않은 파일과 그 내용 해시(지워졌으면 None). 작업 폴더 안만 본다.
    pub fn dirty(&self) -> BTreeMap<String, Option<[u8; 32]>> {
        let mut c = git(&self.top);
        c.args(["status", "--porcelain=v1", "-z", "--untracked-files=all", "--", if self.prefix.is_empty() { "." } else { self.prefix.as_str() }]);
        let Ok(out) = run(&mut c) else { return BTreeMap::new() };
        parse_status(&out)
            .into_iter()
            .map(|p| {
                let h = std::fs::read(self.top.join(&p)).ok().map(|d| Sha256::digest(&d).into());
                (p, h)
            })
            .collect()
    }

    /// 지정한 파일의 HEAD 대비 변경(새 파일 포함). 색인은 건드리지 않는다. 너무 길면 자른다.
    pub fn diff(&self, paths: &[String]) -> String {
        if paths.is_empty() {
            return String::new();
        }
        let tracked: Vec<String> = run_chunked(&self.top, &["ls-tree", "-r", "-z", "--name-only", "--full-tree", "HEAD"], paths).unwrap_or_default().split('\0').filter(|s| !s.is_empty()).map(str::to_string).collect();
        let mut text = String::new();
        if !tracked.is_empty() {
            text = run_chunked(&self.top, &["diff", "HEAD", "--no-color", "--no-ext-diff"], &tracked).unwrap_or_default();
        }
        for p in paths.iter().filter(|p| !tracked.contains(p)) {
            let Ok(body) = std::fs::read(self.top.join(p)) else { continue };
            if body.iter().take(8000).any(|b| *b == 0) {
                text.push_str(&format!("new file {p} (binary, {} B)\n", body.len()));
                continue;
            }
            text.push_str(&format!("--- /dev/null\n+++ b/{p}\n"));
            for line in String::from_utf8_lossy(&body).lines() {
                text.push('+');
                text.push_str(line);
                text.push('\n');
            }
            if text.len() > DIFF_CHARS * 4 {
                break;
            }
        }
        if text.chars().count() <= DIFF_CHARS {
            text
        } else {
            let cut: String = text.chars().take(DIFF_CHARS).collect();
            format!("{cut}\n… (diff가 길어 잘랐습니다. 필요한 파일은 직접 읽으세요)")
        }
    }

    /// 지정한 파일만 커밋한다. 다른 커밋 전 변경과 이미 스테이징된 다른 파일은 담지 않는다. 바뀐 것이 없으면 Ok(None).
    pub fn commit(&self, paths: &[String], message: &str) -> Result<Option<String>, String> {
        if paths.is_empty() {
            return Ok(None);
        }
        let mut add = git(&self.top);
        add.args(["add", "-A", "--ignore-errors", "--pathspec-from-file=-", "--pathspec-file-nul"]);
        run_with_paths(add, paths)?;
        // 실제로 HEAD와 다른 경로만 남긴다(빈 커밋과 '알 수 없는 경로' 오류를 막는다).
        let changed: Vec<String> = run_chunked(&self.top, &["diff", "--cached", "--name-only", "-z", "HEAD"], paths)?.split('\0').filter(|s| !s.is_empty()).map(str::to_string).collect();
        if changed.is_empty() {
            return Ok(None);
        }
        let mut c = git(&self.top);
        c.args(["commit", "--only", "-q", "-m", message, "--pathspec-from-file=-", "--pathspec-file-nul"]);
        run_with_paths(c, &changed)?;
        Ok(Some(run(git(&self.top).args(["rev-parse", "--short", "HEAD"]))?.trim().to_string()))
    }
}

/// `git status --porcelain=v1 -z` → 경로들(이름 바꾸기는 새 이름과 옛 이름 둘 다)
pub fn parse_status(out: &str) -> Vec<String> {
    let mut paths = vec![];
    let mut it = out.split('\0').filter(|s| !s.is_empty());
    while let Some(entry) = it.next() {
        if entry.len() < 4 {
            continue;
        }
        let (code, path) = entry.split_at(3);
        paths.push(path.to_string());
        if code.starts_with('R') || code.starts_with('C') {
            if let Some(old) = it.next() {
                paths.push(old.to_string());
            }
        }
    }
    paths
}

/// How a failed commit can be recovered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitFailure {
    /// Another git process holds a lock: wait and try again.
    Busy,
    /// Git identity or signing is missing: only the person can fix it.
    NeedsUser,
    /// A hook or the change itself was rejected: the lead can fix files and try again.
    Fixable,
}

pub fn commit_failure(err: &str) -> CommitFailure {
    let e = err.to_ascii_lowercase();
    if e.contains("index.lock") || e.contains(".lock': file exists") || e.contains("cannot lock ref") || (e.contains("unable to create") && e.contains(".lock")) || e.contains("another git process") {
        CommitFailure::Busy
    } else if e.contains("tell me who you are") || e.contains("empty ident") || e.contains("user.email") || e.contains("gpg failed") || e.contains("failed to sign") || e.contains("signing failed") || e.contains("dubious ownership") {
        CommitFailure::NeedsUser
    } else {
        CommitFailure::Fixable
    }
}

/// 앞뒤 상태를 비교해 그 사이에 바뀐 경로
pub fn changed_between(before: &BTreeMap<String, Option<[u8; 32]>>, after: &BTreeMap<String, Option<[u8; 32]>>) -> Vec<String> {
    let mut out: Vec<String> = after.iter().filter(|(p, h)| before.get(*p) != Some(h)).map(|(p, _)| p.clone()).collect();
    // 커밋되지 않았던 변경이 되돌려져 깨끗해진 파일도 바뀐 것이다.
    out.extend(before.keys().filter(|p| !after.contains_key(*p)).cloned());
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(dir: &Path, args: &[&str]) {
        let o = std::process::Command::new("git").arg("-C").arg(dir).args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "core.autocrlf=false"]).args(args).output().unwrap();
        assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    }

    fn repo() -> tempfile::TempDir {
        let r = tempfile::tempdir().unwrap();
        g(r.path(), &["init", "-q"]);
        g(r.path(), &["config", "user.name", "t"]);
        g(r.path(), &["config", "user.email", "t@t"]);
        std::fs::write(r.path().join("a.txt"), "a").unwrap();
        std::fs::write(r.path().join("b.txt"), "b").unwrap();
        g(r.path(), &["add", "."]);
        g(r.path(), &["commit", "-qm", "base"]);
        r
    }

    #[test]
    fn commit_failures_are_classified() {
        assert_eq!(commit_failure("fatal: Unable to create 'C:/r/.git/index.lock': File exists."), CommitFailure::Busy);
        assert_eq!(commit_failure("error: cannot lock ref 'HEAD': Unable to create"), CommitFailure::Busy);
        assert_eq!(commit_failure("*** Please tell me who you are."), CommitFailure::NeedsUser);
        assert_eq!(commit_failure("error: gpg failed to sign the data"), CommitFailure::NeedsUser);
        assert_eq!(commit_failure("husky - pre-commit hook exited with code 1 (error)"), CommitFailure::Fixable);
    }

    #[test]
    fn hook_rejection_surfaces_as_error() {
        let r = repo();
        let hooks = r.path().join(".git/hooks");
        std::fs::write(hooks.join("pre-commit"), "#!/bin/sh\necho lint failed >&2\nexit 1\n").unwrap();
        let repo = open(r.path()).unwrap();
        std::fs::write(r.path().join("a.txt"), "changed").unwrap();
        let e = repo.commit(&["a.txt".into()], "m").unwrap_err();
        assert!(e.contains("lint failed"), "{e}");
        assert_eq!(commit_failure(&e), CommitFailure::Fixable);
    }

    #[test]
    fn status_parsing_handles_renames() {
        let out = " M a.txt\0R  new.txt\0old.txt\0?? dir/x [1].txt\0";
        assert_eq!(parse_status(out), vec!["a.txt", "new.txt", "old.txt", "dir/x [1].txt"]);
    }

    #[test]
    fn commit_takes_only_the_named_files() {
        let r = repo();
        let repo = open(r.path()).unwrap();
        // 사용자가 커밋하지 않고 둔 변경
        std::fs::write(r.path().join("b.txt"), "user edit").unwrap();
        let before = repo.dirty();
        // 워커가 바꾼 파일
        std::fs::write(r.path().join("a.txt"), "worker").unwrap();
        std::fs::write(r.path().join("new [x].txt"), "n").unwrap();
        let after = repo.dirty();
        let changed = changed_between(&before, &after);
        assert_eq!(changed, vec!["a.txt", "new [x].txt"]);
        assert!(repo.diff(&changed).contains("+worker"));
        let sha = repo.commit(&changed, "feat: worker").unwrap();
        assert!(sha.is_some());
        let left = repo.dirty();
        assert_eq!(left.keys().cloned().collect::<Vec<_>>(), vec!["b.txt"], "사용자 변경은 남는다");
        assert_eq!(repo.commit(&changed, "again").unwrap(), None, "바뀐 것이 없으면 커밋하지 않는다");
    }

    #[test]
    fn deletions_are_committed() {
        let r = repo();
        let repo = open(r.path()).unwrap();
        std::fs::remove_file(r.path().join("a.txt")).unwrap();
        assert!(repo.commit(&["a.txt".to_string()], "chore: remove").unwrap().is_some());
        assert!(repo.dirty().is_empty());
    }

    #[test]
    fn subfolder_paths_get_the_prefix_and_exclude_is_added_once() {
        let r = repo();
        std::fs::create_dir_all(r.path().join("sub")).unwrap();
        let repo = open(&r.path().join("sub")).unwrap();
        assert_eq!(repo.prefix, "sub/");
        exclude_workspace_dir(r.path());
        exclude_workspace_dir(r.path());
        let ex = std::fs::read_to_string(r.path().join(".git/info/exclude")).unwrap();
        assert_eq!(ex.matches(".elysium/").count(), 1);
        std::fs::create_dir_all(r.path().join(".elysium/worktrees")).unwrap();
        std::fs::write(r.path().join(".elysium/worktrees/x"), "x").unwrap();
        assert!(open(r.path()).unwrap().dirty().is_empty(), "작업 공간 폴더는 변경으로 보이지 않는다");
    }

    #[test]
    fn folder_without_commits_is_not_a_repo_for_commits() {
        let r = tempfile::tempdir().unwrap();
        g(r.path(), &["init", "-q"]);
        assert!(open(r.path()).is_none());
        assert!(open(tempfile::tempdir().unwrap().path()).is_none());
    }
}
