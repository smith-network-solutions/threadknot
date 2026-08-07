// Prevents an extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Behavioral probe: exercise the exact production notification backend
    // without opening a window or starting Threadknot's server. This is useful on
    // fresh Linux desktops and after installing a Windows build.
    if std::env::args_os().any(|arg| arg == "--test-notification") {
        match threadknot_lib::notifications::send_test() {
            Ok(receipt) => {
                eprintln!("Threadknot notification accepted: {receipt:?}");
                // The GNOME notification source is tied to this process's
                // D-Bus connection. Keep the behavioral probe alive long
                // enough to verify the actual banner before exiting.
                #[cfg(target_os = "linux")]
                std::thread::sleep(std::time::Duration::from_secs(30));
                return;
            }
            Err(error) => {
                eprintln!("Threadknot notification failed: {error}");
                std::process::exit(1);
            }
        }
    }

    // webkit2gtk crashes with "Error 71 dispatching to Wayland display" on
    // NVIDIA + Wayland without this. Do NOT also disable compositing mode —
    // software rendering makes the whole UI flicker/repaint constantly.
    #[cfg(target_os = "linux")]
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }
    threadknot_lib::run()
}
