use objc::{msg_send, runtime::Object, sel, sel_impl};
use std::os::raw::c_void;

const KCG_DESKTOP_ICON_WINDOW_LEVEL: i32 = -2147483644; // kCGDesktopIconWindowLevel

pub fn attach(ns_window: *mut c_void) {
    unsafe {
        let ns_window: *mut Object = ns_window as *mut Object;
        println!("Setting window level to kCGDesktopWindowLevel");
        let _: () = msg_send![ns_window, setLevel: KCG_DESKTOP_WINDOW_LEVEL]; // 确保使用正确的常量
        println!(
            "Setting window collection behavior to NSWindowCollectionBehaviorCanJoinAllSpaces"
        );
        // NSWindowCollectionBehaviorCanJoinAllSpaces
        let _: () = msg_send![ns_window, setCollectionBehavior: 1 << 7];

        // 可能需要设置其他属性
        // 设置为透明
        let _: () = msg_send![ns_window, setOpaque: NO];
        let _: () = msg_send![ns_window, setBackgroundColor: NSColor::clearColor()];
        // 设置背景颜色为透明
    }
}

pub fn detach(ns_window: *mut c_void) {
    unsafe {
        let ns_window: *mut Object = ns_window as *mut Object;
        println!("Resetting window level to NSNormalWindowLevel");
        let _: () = msg_send![ns_window, setLevel: 0]; // NSNormalWindowLevel
        println!("Resetting window collection behavior");
        let _: () = msg_send![ns_window, setCollectionBehavior: 0];
    }
}
