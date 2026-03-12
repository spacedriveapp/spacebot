#![cfg(target_os = "macos")]

use swift_rs::{swift, Bool, Int};

pub type NSObject = *mut std::ffi::c_void;

swift!(pub fn set_titlebar_style(window: &NSObject, is_fullscreen: Bool));
swift!(pub fn lock_app_theme(theme_type: Int));
