// 데스크톱에서 화면만 미리 볼 때 쓴다(실제 앱은 Android).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    elysium_remote_lib::run()
}
