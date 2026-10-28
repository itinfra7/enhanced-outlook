// 릴리스 빌드에서 추가 콘솔 창을 띄우지 않는다.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    enhanced_outlook_lib::run()
}
