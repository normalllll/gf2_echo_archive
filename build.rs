//! Embeds the application icon into the Windows executable. The icon gets
//! resource id 1, which is also what GPUI loads for the window and taskbar.

fn main() {
    println!("cargo:rerun-if-changed=resources/icon/app-icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new()
            .set_icon("resources/icon/app-icon.ico")
            .compile()
            .expect("无法嵌入程序图标");
    }
}
