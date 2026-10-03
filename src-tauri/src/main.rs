// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // An AppImage's mount stays out of the node and the background part (lib.rs).
    freebank::keep_inherited_files_from_children();
    // With "Keep your phone connected when FreeBank is closed", the app starts itself again without a
    // window to keep the phone link up (phone/background.rs).
    // args_os: an argument that isn't UTF-8 must not stop the app from starting (code review 8).
    let args: Vec<String> = std::env::args_os().map(|a| a.to_string_lossy().into_owned()).collect();
    if let Some((dir, light)) = freebank::phone_background_requested(&args) {
        std::process::exit(freebank::phone_background_main(dir, light));
    }
    freebank::run()
}
