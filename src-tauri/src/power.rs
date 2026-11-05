//! 취침 모드: PC를 쓰지 않는 동안 화면을 끄고(앱의 그리기는 화면 쪽에서 멈춘다) 전기를 아낀다.
//! 작업이 도는 동안에는 Windows가 절전에 들어가지 않게 붙잡아 작업 속도는 그대로 둔다. 작업이 없으면 평소처럼 잠들 수 있다.
//! 창 테스트가 도는 동안에는 모니터를 켜 둔다. 모니터가 꺼지면 Windows가 창 그리기·화면 갱신을 멈추거나 늦춰 게임·GUI 테스트가 멈추고 스크린샷이 검게 나온다.

use crate::orchestrator::AppState;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

static SLEEPING: AtomicBool = AtomicBool::new(false);
/// 취침할 때 모니터를 끄기로 했는지(창 테스트가 끝나면 다시 끈다)
static MONITOR_OFF: AtomicBool = AtomicBool::new(false);
static WATCHER: OnceLock<std::thread::Thread> = OnceLock::new();

#[derive(Clone, Copy, PartialEq, Eq)]
enum Hold {
    None,
    /// 작업 중: 시스템만 깨어 있게(화면은 꺼져도 된다)
    System,
    /// 창 테스트 중: 화면도 켜 둔다
    Display,
}

#[cfg(windows)]
fn hold(h: Hold) {
    use windows_sys::Win32::System::Power::{SetThreadExecutionState, ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED};
    let flags = match h {
        Hold::None => ES_CONTINUOUS,
        Hold::System => ES_CONTINUOUS | ES_SYSTEM_REQUIRED,
        Hold::Display => ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED,
    };
    // SAFETY: 인자만 받는 Win32 호출. 상태는 이 호출을 한 스레드에 묶이므로 감시 스레드에서만 부른다.
    unsafe {
        SetThreadExecutionState(flags);
    }
}

#[cfg(not(windows))]
fn hold(_h: Hold) {}

/// 모니터를 끄거나(2) 켠다(-1). 마우스나 키를 건드려도 Windows가 다시 켠다.
#[cfg(windows)]
fn monitor_power(off: bool) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, HWND_BROADCAST, SC_MONITORPOWER, WM_SYSCOMMAND};
    // SAFETY: 메시지를 보내기만 하고 기다리지 않는다(SendMessage는 응답하지 않는 창에서 멈출 수 있다).
    unsafe {
        PostMessageW(HWND_BROADCAST, WM_SYSCOMMAND, SC_MONITORPOWER as usize, if off { 2 } else { -1isize });
    }
}

#[cfg(not(windows))]
fn monitor_power(_off: bool) {}

/// 취침하며 모니터를 끄기로 했다. 창 테스트가 도는 중이면 끝난 뒤에 끈다(감시 스레드가 처리).
pub fn monitor_off(st: &AppState) {
    MONITOR_OFF.store(true, Ordering::Relaxed);
    if !window_test_running(st) {
        monitor_power(true);
    }
}

fn window_test_running(st: &AppState) -> bool {
    st.tests.running_count() > 0
}

/// 취침 모드를 켜거나 끈다. 처음 켤 때 감시 스레드를 하나 띄운다(실행 상태는 스레드에 묶이기 때문).
pub fn set_sleep(st: Arc<AppState>, on: bool) {
    SLEEPING.store(on, Ordering::Relaxed);
    if !on {
        MONITOR_OFF.store(false, Ordering::Relaxed);
    }
    let watcher = WATCHER.get_or_init(|| {
        std::thread::Builder::new()
            .name("elysium-power".into())
            .spawn(move || {
                let mut held = Hold::None;
                let mut windows = false;
                loop {
                    let sleeping = SLEEPING.load(Ordering::Relaxed);
                    let busy = !st.tasks.lock().unwrap_or_else(|p| p.into_inner()).is_empty();
                    let testing = window_test_running(&st);
                    let want = match (sleeping, testing, busy) {
                        (false, _, _) => Hold::None,
                        (true, true, _) => Hold::Display,
                        (true, false, true) => Hold::System,
                        (true, false, false) => Hold::None,
                    };
                    if want != held {
                        hold(want);
                        held = want;
                    }
                    // 꺼 둔 모니터: 창 테스트가 시작되면 켜고, 모두 끝나면 다시 끈다.
                    if sleeping && MONITOR_OFF.load(Ordering::Relaxed) && testing != windows {
                        monitor_power(!testing);
                    }
                    windows = testing;
                    // 취침 중에는 창 테스트 시작을 곧 알아채도록 자주 본다(잠금 한 번과 목록 복사뿐이라 가볍다).
                    std::thread::park_timeout(Duration::from_secs(if sleeping { 2 } else { 60 }));
                }
            })
            .map(|h| h.thread().clone())
            .unwrap_or_else(|_| std::thread::current())
    });
    // 켜고 끈 것을 바로 반영한다(다음 확인까지 최대 60초를 기다리지 않게).
    watcher.unpark();
}
