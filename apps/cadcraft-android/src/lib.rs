//! CadCraft on Android (arm64-v8a): the real engine and egui shell, running
//! on winit's NativeActivity backend.
//!
//! # What this crate is
//!
//! A shell around the *unmodified* CadCraft crates — `cadcraft-engine` (the
//! document model and command registry), `cadcraft-io` (PSD and flat codecs)
//! and `cadcraft-ui-egui` (the egui UI). Nothing in those crates is patched.
//! This crate supplies only what `apps/cadcraft` would normally supply: the
//! [`Services`] implementation, the window options, and the entry point.
//!
//! # Why a separate crate, and not a feature on `apps/cadcraft`
//!
//! That crate cannot build for Android as written: it depends on `rfd` (native
//! file dialogs), `arboard` (OS clipboard) and `open`, all of which are
//! desktop-only. `rfd` fails with `the trait bound FileDialog:
//! FilePickerDialogImpl is not satisfied` and `arboard` with `cannot find Set in
//! module platform` — they have no Android backends at all.
//!
//! Those uses are confined to that crate's `services.rs`, and
//! `cadcraft_ui_egui::Services` is a plain struct of `Option` closures that
//! derives `Default`. So the fix is not to patch the app but to build a
//! platform that supplies what it can and omits the rest — which is exactly what
//! `Services` was shaped for.
//!
//! # What Android cannot do yet
//!
//! These are honest gaps, not oversights:
//!
//! - **No file dialogs.** `file_dialog` stays `None`, so Open… and Save As… do
//!   nothing. Android needs the Storage Access Framework, i.e. a JNI bridge to
//!   `Intent.ACTION_OPEN_DOCUMENT` plus `onActivityResult` plumbing. Any path
//!   already reachable in the app's own data directory still works through
//!   `write`/`export`/`import`.
//! - **No clipboard image transfer.** `arboard` has no Android backend.
//! - **No stylus pressure, tilt or rotation.** `crates/tablet` implements macOS
//!   AppKit and Linux X11 XInput2 only, so `#[cfg]`s it to a no-op here and a
//!   pen degrades to ordinary touch. Android does expose those values on
//!   `MotionEvent`, so a backend is writable — but it does not exist yet.
//! - **No crash recovery.** `recover`/`autosave` are unwired for now.
//!
//! Editing itself is unaffected: document model, history, the 500+ command
//! registry, the CPU compositor and the wgpu GPU canvas are all
//! platform-neutral.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex, OnceLock};

use cadcraft_engine::Session;
use cadcraft_ui_egui::{CadApp, Services};

mod clipboard;
mod intents;
mod modifiers;

/// The app's private data directory, captured in `android_main` before any
/// service closure runs.
///
/// `internal_data_path()` needs no runtime permission — it is the app's own
/// sandbox — which is why preferences and exports land here rather than in
/// shared storage.
static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

fn prefs_file() -> Option<PathBuf> {
    DATA_DIR.get().map(|d| d.join("preferences.json"))
}

/// Mirrors the desktop shell's `export_options`
/// (`apps/cadcraft/src/services.rs`), so Export As behaves identically.

/// Encode and write on a worker thread (`Services::save_file`).
///
/// Mirrors the desktop shell's `save_through`. With background jobs on the app uses this instead
/// of `export` + `write` (`write_document`), so a large save cannot stall the event loop — which
/// on Android is the difference between a progress tab and the watchdog offering "Force close"
/// while the save is still running.

/// The platform-neutral half of the desktop `services::native()`.
///
/// Everything omitted here is omitted deliberately — see the crate docs. The
/// codecs are wired identically, so opening and exporting real files works.
fn android_services() -> Services {
    Services {
        // Android has a real file system; `no_file_system` stays false. The other fields are
        // left None: the app falls back to its own file handling, the same way the web build
        // does when a host supplies no native dialog.
        no_file_system: false,
        ..Default::default()
    }
}



/// A minimal file logger, so a sideloaded APK leaves a readable trace.
///
/// `logcat` is only reachable over adb; a file in the app's own data directory
/// can be pulled with any file manager, which is what a sideloaded build needs.
struct FileLogger {
    sink: Mutex<Option<std::fs::File>>,
}

impl log::Log for FileLogger {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }
    fn log(&self, record: &log::Record) {
        if let Ok(mut guard) = self.sink.lock() {
            if let Some(f) = guard.as_mut() {
                use std::io::Write;
                let _ = writeln!(f, "[{}] {} {}", record.level(), record.target(), record.args());
            }
        }
    }
    fn flush(&self) {}
}

static LOGGER: OnceLock<FileLogger> = OnceLock::new();

fn init_logging(dir: &Path) {
    let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("cadcraft.log"))
    else {
        return;
    };
    let logger = LOGGER.get_or_init(|| FileLogger {
        sink: Mutex::new(None),
    });
    if let Ok(mut guard) = logger.sink.lock() {
        *guard = Some(file);
    }
    if log::set_logger(logger).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
}

/// The Android entry point.
///
/// `android-activity` spawns a dedicated thread for this and calls it with the
/// activity handle; `#[unsafe(no_mangle)]` with the Rust ABI is the required
/// shape (edition 2024 requires the `unsafe(..)` wrapper around `no_mangle`).
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub fn android_main(app: winit::platform::android::activity::AndroidApp) {
    let data_dir = app
        .internal_data_path()
        .unwrap_or_else(|| PathBuf::from("."));
    let _ = std::fs::create_dir_all(&data_dir);
    let _ = DATA_DIR.set(data_dir.clone());
    // The system clipboard goes through the same Activity handle as the rest of the
    // platform (see `clipboard`).
    clipboard::install(&app);
    intents::install(&app);
    // The app's external files directory: always readable and writable with no
    // permission, so the picker can fall back to it for both open and save.
    init_logging(&data_dir);
    log::info!(
        "CadCraft {} starting on Android; data dir {}",
"0.15.1",
        data_dir.display()
    );

    // Before the window opens: the font scan is one-shot and the first one to run wins, so the
    // app's own Fonts folder has to be registered here or it is never seen (`file_picker`).
    let mut options = eframe::NativeOptions::default();
    // Required on Android: eframe hands this to
    // `EventLoopBuilderExtAndroid::with_android_app` and errors without it.
    options.android_app = Some(app);

    let result = eframe::run_native(
        "CadCraft",
        options,
        Box::new(|cc| {
            let mut services = android_services();
            let mut app = CadApp::new(Session::new(), services);
            // Long commands and file opens as background jobs with progress and Cancel (#210).
            //
            // The desktop turns this on and the default is OFF — "tests and scripts rely on" the
            // inline path. Leaving it unset ran every long operation inline on the event-loop
            // thread: the loop stopped draining input, and Android's watchdog offered "Force
            // close" after ~5s while the work carried on and finished anyway, which is exactly the
            // ANR the tablet showed. With this on, `write_document` also uses `Services::save_file`
            // (worker thread) instead of `export` + `write` here — both halves are needed.
            // Put the canvas on the GPU when the device gave us a wgpu device;
            // otherwise the engine's CPU compositor carries it.
            if let Some(rs) = cc.wgpu_render_state.clone() {
                log::info!("wgpu adapter: {:?}", rs.adapter.get_info());
                app.set_wgpu(&rs, None);
            } else {
                log::warn!("no wgpu render state; the canvas will composite on the CPU");
            }
            Ok(Box::new(Shell { inner: app, modifiers: modifiers::Tracked::default(), pending_launch: intents::launch() }))
        }),
    );

    match result {
        Ok(()) => log::info!("CadCraft exited normally"),
        Err(e) => log::error!("CadCraft failed to run: {e}"),
    }
}

/// Wraps the real app so the file picker can draw on top of it.
///
/// The same shape as the web build's `WebShell` (`apps/cadcraft-web/src/web.rs`):
/// `PhotocraftApp` stays untouched, and the platform-specific layer hangs off the
/// outside. Every trait method is forwarded, so the app sees no difference.
struct Shell {
    inner: CadApp,
    /// Modifier keys folded back into the frame's events; Android reports none (see `modifiers`).
    modifiers: modifiers::Tracked,
    pending_launch: Option<intents::LaunchIntent>,
}

impl eframe::App for Shell {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if let Some(launch) = self.pending_launch.take() {
            match launch {
                intents::LaunchIntent::OpenFile(path) => { self.inner.open_path(&path); }
                intents::LaunchIntent::ImportText(text) => {
                    let path = std::env::temp_dir().join("shared.txt");
                    if std::fs::write(&path, text).is_ok() { self.inner.open_path(&path); }
                }
            }
        }
        self.inner.logic(ctx);
    }

    /// Forwarded because `PhotocraftApp` implements it and it is not just cosmetic:
    /// it runs `shortcuts::clipboard_keys` and appends `take_synthetic_step()` to the
    /// frame's events. Dropping it silently loses both — the same gap the VectorCraft
    /// port had to fix.
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        // Before the app's own hook, so it sees the frame a desktop backend would have
        // produced. Android's winit never sends `ModifiersChanged`, so without this every
        // Ctrl shortcut, Shift+click and Alt keytip is dead (see `modifiers`).
        for event in &mut raw.events {
            if let egui::Event::Key { key, .. } = event
                && *key == egui::Key::BrowserBack
            {
                *key = egui::Key::Escape;
            }
        }
        let modifiers = self.modifiers.apply(raw);
        // Then the clipboard keys: egui-winit could not turn them into Copy/Cut/Paste
        // because the backend never reports Modifiers, so they are still ordinary key
        // presses here (see `clipboard::shortcuts`).
        clipboard::shortcuts(raw);
        self.inner.raw_input_hook(ctx, raw);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.inner.ui(ui);
        // After the app: this is the one place that sees egui's CopyText before egui-winit
        // consumes it, and puts it on the system clipboard (see `clipboard::publish`).
        clipboard::publish(ui.ctx());
    }




}
