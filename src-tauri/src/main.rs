// 릴리스 빌드에서 Windows 콘솔 창이 뜨지 않도록 한다.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    elysium_lib::run()
}
