//! Elysium 데스크톱 앱 코어: 여러 AI CLI를 엮어 완성된 답을 낸다.

mod api_agent;
mod apis;
mod audit;
mod commands;
mod db;
mod engines;
mod error;
mod intent;
mod jev;
mod memory;
mod orchestrator;
mod pricing;
mod remote;
mod secrets;
mod settings;
mod skills;
mod store;
mod teamwork;

use orchestrator::AppState;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let db = db::Db::open(&data_dir.join("elysium.db"))?;
            store::expire_pending_approvals(&db)?;
            let _ = audit::prune(&db, 90);
            if let Err(e) = skills::seed(&data_dir) {
                eprintln!("예시 스킬을 만들지 못했습니다: {e}");
            }
            let s = settings::load(&db)?;
            // 일부 API 게이트웨이(NVIDIA 등)는 User-Agent 없는 요청을 막는다.
            let http = reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(10))
                .user_agent(concat!("Elysium/", env!("CARGO_PKG_VERSION")))
                .build()?;
            let models = s.models.clone();
            let state = Arc::new(AppState {
                db,
                http,
                data_dir,
                settings: RwLock::new(s),
                engines: RwLock::new(vec![]),
                tasks: Mutex::new(HashMap::new()),
                agents: Mutex::new(HashMap::new()),
                steers: Mutex::new(HashMap::new()),
                approvals: Mutex::new(HashMap::new()),
            });
            app.manage(state.clone());
            // 지난 실행에서 끝내지 못한 병렬 작업 사본을 지운다(시작할 때는 도는 작업이 없다).
            let leftovers = state.data_dir.join("teamwork");
            std::thread::spawn(move || {
                let _ = std::fs::remove_dir_all(leftovers);
            });
            // 원격 제어를 켜 두었으면 시작할 때 서버를 띄운다.
            if remote::load_config(&state.db).map(|c| c.enabled).unwrap_or(false) {
                let (h, s) = (app.handle().clone(), state.clone());
                tauri::async_runtime::spawn(async move {
                    if let Err(e) = remote::start(h, s).await {
                        eprintln!("원격 제어를 시작하지 못했습니다: {e}");
                    }
                });
            }
            // 설치된 CLI는 시작할 때 한 번 찾는다(설정에서 다시 찾을 수 있음).
            tauri::async_runtime::spawn(async move {
                let found = engines::detect(&models).await;
                *state.engines.write().unwrap_or_else(|p| p.into_inner()) = found;
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::app_info,
            commands::settings_get,
            commands::settings_update,
            commands::engines_list,
            commands::engines_refresh,
            commands::secrets_status,
            commands::secret_shape,
            commands::secret_set,
            commands::secret_clear,
            commands::api_test,
            commands::jev_test,
            commands::conversation_list,
            commands::conversation_messages,
            commands::conversation_rename,
            commands::conversation_delete,
            commands::conversation_set_workspace,
            commands::pick_folder,
            commands::chat_send,
            commands::task_cancel,
            commands::agent_cancel,
            commands::task_steer,
            commands::cancel_all,
            commands::approval_list,
            commands::approval_decide,
            commands::skills_list,
            commands::skill_toggle,
            commands::plugin_toggle,
            commands::skill_create,
            commands::open_path,
            commands::open_url,
            commands::usage_today,
            commands::usage_summary,
            commands::pricing_list,
            commands::conversation_markdown,
            commands::conversation_export,
            commands::memory_get,
            commands::memory_clear,
            commands::remote_status,
            commands::remote_configure,
            commands::remote_pair_start,
            commands::remote_pair_cancel,
            commands::remote_revoke,
            commands::data_delete_all,
        ])
        .run(tauri::generate_context!())
        .expect("Elysium 실행 실패");
}
