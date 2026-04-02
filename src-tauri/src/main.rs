// Prevents an additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // WebKitGTK compatibility: set env vars before GTK/WebKit initialises.
    // Only set if the user hasn't already overridden them.
    //
    // SAFETY: called before any threads are spawned (pre-GTK init).
    // Will require `unsafe` block when migrating to Rust edition 2024.
    #[cfg(target_os = "linux")]
    {
        use std::env;
        // Fixes EGL_BAD_PARAMETER and GBM buffer failures on AMD RDNA3+,
        // some Intel iGPUs, and NVIDIA with Wayland. Falls back to the
        // SHM renderer which is visually identical for a form-based UI.
        if env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
            env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
        }
        // Fixes blank/flickering windows on NVIDIA 545+ drivers with
        // Wayland explicit sync. Falls back to implicit sync.
        if env::var_os("__NV_DISABLE_EXPLICIT_SYNC").is_none() {
            env::set_var("__NV_DISABLE_EXPLICIT_SYNC", "1");
        }
    }

    histv_lib::run()
}
