//! 창을 띄우는 실행(창 테스트)의 대기열. 앱 전체에 하나이고, 먼저 온 순서대로 자리가 나면 시작한다.

use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestItem {
    pub id: u64,
    pub task: String,
    pub key: String,
    pub command: String,
    /// 실행을 시작한 시각(ms). 기다리는 중이면 None.
    #[serde(default)]
    pub started_at: Option<i64>,
}

/// 끝난 테스트 하나(최근 몇 개만 둔다)
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestDone {
    pub id: u64,
    pub task: String,
    pub key: String,
    pub command: String,
    pub ok: bool,
    /// 결과 첫 줄(종료 코드·취소 이유)
    pub summary: String,
    pub started_at: Option<i64>,
    pub ended_at: i64,
}

/// 테스트마다 보관하는 로그 줄 수(넘으면 앞에서부터 버린다)
pub const LOG_LINES: usize = 4000;
/// 끝난 테스트를 몇 개까지 보여 줄지
const RECENT: usize = 8;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueView {
    pub capacity: usize,
    pub running: Vec<TestItem>,
    pub waiting: Vec<TestItem>,
    pub recent: Vec<TestDone>,
}

struct State {
    capacity: usize,
    running: Vec<TestItem>,
    waiting: VecDeque<TestItem>,
    stops: HashMap<u64, CancellationToken>,
    recent: VecDeque<TestDone>,
    logs: HashMap<u64, VecDeque<String>>,
}

type OnChange = Box<dyn Fn(QueueView) + Send + Sync>;

pub struct TestQueue {
    state: Mutex<State>,
    notify: Notify,
    next: AtomicU64,
    on_change: Mutex<Option<OnChange>>,
}

/// 들고 있는 동안 자리 하나를 차지한다. 버리면 다음 항목이 시작된다.
pub struct TestGuard {
    queue: Arc<TestQueue>,
    id: u64,
}

impl Drop for TestGuard {
    fn drop(&mut self) {
        {
            let mut s = self.queue.lock();
            s.running.retain(|i| i.id != self.id);
            s.stops.remove(&self.id);
        }
        self.queue.changed();
    }
}

/// 기다리다 취소되거나 버려지면 줄에서 뺀다.
struct WaitSpot<'a> {
    queue: &'a TestQueue,
    id: u64,
    armed: bool,
}

impl Drop for WaitSpot<'_> {
    fn drop(&mut self) {
        if self.armed {
            {
                let mut s = self.queue.lock();
                s.waiting.retain(|i| i.id != self.id);
                s.stops.remove(&self.id);
            }
            self.queue.changed();
        }
    }
}

impl TestQueue {
    pub fn new(capacity: usize) -> Self {
        TestQueue {
            state: Mutex::new(State { capacity: capacity.max(1), running: vec![], waiting: VecDeque::new(), stops: HashMap::new(), recent: VecDeque::new(), logs: HashMap::new() }),
            notify: Notify::new(),
            next: AtomicU64::new(1),
            on_change: Mutex::new(None),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn changed(&self) {
        self.notify.notify_waiters();
        let view = self.snapshot();
        if let Some(f) = self.on_change.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
            f(view);
        }
    }

    pub fn set_on_change(&self, f: impl Fn(QueueView) + Send + Sync + 'static) {
        *self.on_change.lock().unwrap_or_else(|p| p.into_inner()) = Some(Box::new(f));
    }

    pub fn next_id(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }

    /// 늘리면 바로 다음 항목이 시작되고, 줄여도 실행 중인 테스트는 끊지 않는다.
    pub fn set_capacity(&self, n: usize) {
        self.lock().capacity = n.max(1);
        self.changed();
    }

    /// 지금 열려 있는 창 테스트 수(목록을 복사하지 않는다)
    pub fn running_count(&self) -> usize {
        self.lock().running.len()
    }

    pub fn snapshot(&self) -> QueueView {
        let s = self.lock();
        QueueView { capacity: s.capacity, running: s.running.clone(), waiting: s.waiting.iter().cloned().collect(), recent: s.recent.iter().rev().cloned().collect() }
    }

    /// 테스트 로그 한 줄을 보관한다.
    pub fn push_log(&self, id: u64, line: &str) {
        let mut s = self.lock();
        let log = s.logs.entry(id).or_default();
        log.push_back(line.to_string());
        while log.len() > LOG_LINES {
            log.pop_front();
        }
    }

    /// 보관 중인 로그(실행 중이거나 최근에 끝난 테스트)
    pub fn log(&self, id: u64) -> Vec<String> {
        self.lock().logs.get(&id).map(|l| l.iter().cloned().collect()).unwrap_or_default()
    }

    /// 끝난 테스트를 기록한다. 오래된 기록의 로그는 버린다.
    pub fn finish(&self, item: &TestItem, started_at: Option<i64>, ok: bool, summary: &str) {
        {
            let mut s = self.lock();
            s.recent.push_back(TestDone { id: item.id, task: item.task.clone(), key: item.key.clone(), command: item.command.clone(), ok, summary: summary.chars().take(200).collect(), started_at, ended_at: now_ms() });
            while s.recent.len() > RECENT {
                if let Some(old) = s.recent.pop_front() {
                    s.logs.remove(&old.id);
                }
            }
        }
        self.changed();
    }

    /// 대기 순번(1부터). 이미 시작했거나 없으면 None.
    pub fn position(&self, id: u64) -> Option<usize> {
        self.lock().waiting.iter().position(|i| i.id == id).map(|p| p + 1)
    }

    /// 사용자가 테스트 하나만 끈다(기다리는 중이면 줄에서 빠진다).
    pub fn cancel_test(&self, id: u64) -> bool {
        match self.lock().stops.get(&id) {
            Some(t) => {
                t.cancel();
                true
            }
            None => false,
        }
    }

    /// 자리가 날 때까지 기다린다. 취소되면 None.
    pub async fn acquire(self: &Arc<Self>, item: TestItem, cancel: &CancellationToken) -> Option<TestGuard> {
        let id = item.id;
        {
            let mut s = self.lock();
            s.stops.insert(id, cancel.clone());
            s.waiting.push_back(item);
        }
        self.changed();
        let mut spot = WaitSpot { queue: self, id, armed: true };
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let started = {
                let mut s = self.lock();
                let head = s.waiting.front().is_some_and(|i| i.id == id);
                if head && s.running.len() < s.capacity {
                    let mut it = s.waiting.pop_front().expect("head");
                    it.started_at = Some(now_ms());
                    s.running.push(it);
                    true
                } else {
                    false
                }
            };
            if started {
                spot.armed = false;
                self.changed();
                return Some(TestGuard { queue: self.clone(), id });
            }
            tokio::select! {
                _ = cancel.cancelled() => return None,
                _ = &mut notified => {}
            }
        }
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn item(q: &TestQueue, name: &str) -> TestItem {
        TestItem { id: q.next_id(), task: "t".into(), key: name.into(), command: name.into(), started_at: None }
    }

    async fn soon<T>(f: impl std::future::Future<Output = T>) -> T {
        tokio::time::timeout(Duration::from_secs(2), f).await.expect("시간 초과")
    }

    #[tokio::test]
    async fn fifo_order_with_one_slot() {
        let q = Arc::new(TestQueue::new(1));
        let c = CancellationToken::new();
        let first = soon(q.acquire(item(&q, "a"), &c)).await.unwrap();
        let (qb, cb, ib) = (q.clone(), c.clone(), item(&q, "b"));
        let b = tokio::spawn(async move { qb.acquire(ib, &cb).await.map(|g| (g, "b")) });
        tokio::task::yield_now().await;
        while q.snapshot().waiting.is_empty() {
            tokio::task::yield_now().await;
        }
        let (qc, cc, ic) = (q.clone(), c.clone(), item(&q, "c"));
        let c_task = tokio::spawn(async move { qc.acquire(ic, &cc).await.map(|g| (g, "c")) });
        while q.snapshot().waiting.len() < 2 {
            tokio::task::yield_now().await;
        }
        assert_eq!(q.position(q.snapshot().waiting[1].id), Some(2));
        assert_eq!(q.snapshot().running.len(), 1);
        drop(first);
        let (gb, who) = soon(b).await.unwrap().unwrap();
        assert_eq!(who, "b", "먼저 온 항목이 먼저");
        assert_eq!(q.snapshot().waiting.len(), 1);
        drop(gb);
        assert_eq!(soon(c_task).await.unwrap().unwrap().1, "c");
    }

    #[tokio::test]
    async fn raising_capacity_starts_the_next_waiter() {
        let q = Arc::new(TestQueue::new(1));
        let c = CancellationToken::new();
        let _a = soon(q.acquire(item(&q, "a"), &c)).await.unwrap();
        let (qb, cb, ib) = (q.clone(), c.clone(), item(&q, "b"));
        let b = tokio::spawn(async move { qb.acquire(ib, &cb).await.is_some() });
        while q.snapshot().waiting.is_empty() {
            tokio::task::yield_now().await;
        }
        q.set_capacity(2);
        assert!(soon(b).await.unwrap());
    }

    #[tokio::test]
    async fn lowering_capacity_keeps_running_tests() {
        let q = Arc::new(TestQueue::new(2));
        let c = CancellationToken::new();
        let _a = soon(q.acquire(item(&q, "a"), &c)).await.unwrap();
        let b = soon(q.acquire(item(&q, "b"), &c)).await.unwrap();
        q.set_capacity(1);
        assert_eq!(q.snapshot().running.len(), 2, "실행 중인 테스트는 끊지 않는다");
        let (qc, cc, ic) = (q.clone(), c.clone(), item(&q, "c"));
        let waiter = tokio::spawn(async move { qc.acquire(ic, &cc).await.is_some() });
        while q.snapshot().waiting.is_empty() {
            tokio::task::yield_now().await;
        }
        drop(b);
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(!waiter.is_finished(), "남은 하나가 끝날 때까지 자리가 없다");
        drop(_a);
        assert!(soon(waiter).await.unwrap());
    }

    #[tokio::test]
    async fn cancel_while_waiting_leaves_the_queue() {
        let q = Arc::new(TestQueue::new(1));
        let c = CancellationToken::new();
        let a = soon(q.acquire(item(&q, "a"), &c)).await.unwrap();
        let stop = c.child_token();
        let it = item(&q, "b");
        let id = it.id;
        let (qb, sb) = (q.clone(), stop.clone());
        let b = tokio::spawn(async move { qb.acquire(it, &sb).await.is_some() });
        while q.snapshot().waiting.is_empty() {
            tokio::task::yield_now().await;
        }
        assert!(q.cancel_test(id));
        assert!(!soon(b).await.unwrap());
        assert!(q.snapshot().waiting.is_empty());
        drop(a);
        assert!(soon(q.acquire(item(&q, "c"), &c)).await.is_some(), "빠진 뒤에도 줄이 막히지 않는다");
    }

    #[test]
    fn logs_are_capped_and_dropped_with_old_history() {
        let q = TestQueue::new(1);
        let first = item(&q, "a");
        for i in 0..LOG_LINES + 5 {
            q.push_log(first.id, &i.to_string());
        }
        let log = q.log(first.id);
        assert_eq!(log.len(), LOG_LINES);
        assert_eq!(log[0], "5", "오래된 줄부터 버린다");
        q.finish(&first, None, true, "종료 코드 0");
        assert_eq!(q.snapshot().recent[0].summary, "종료 코드 0");
        for _ in 0..RECENT {
            let it = item(&q, "b");
            q.finish(&it, None, false, "x");
        }
        assert!(q.log(first.id).is_empty(), "기록에서 밀려나면 로그도 버린다");
        assert_eq!(q.snapshot().recent.len(), RECENT);
    }

    #[tokio::test]
    async fn guard_drop_releases_slot_and_reports_changes() {
        let q = Arc::new(TestQueue::new(1));
        let seen = Arc::new(Mutex::new(vec![]));
        let s2 = seen.clone();
        q.set_on_change(move |v| s2.lock().unwrap().push((v.running.len(), v.waiting.len())));
        let c = CancellationToken::new();
        let a = soon(q.acquire(item(&q, "a"), &c)).await.unwrap();
        drop(a);
        assert_eq!(q.snapshot().running.len(), 0);
        assert_eq!(seen.lock().unwrap().last(), Some(&(0, 0)));
        assert!(seen.lock().unwrap().contains(&(1, 0)));
    }
}
