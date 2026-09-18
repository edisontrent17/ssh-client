//! Optional macOS prototype. The C API is pinned to Ghostty 1.3.1.
//! Native handles are main-thread-only (Rc makes them !Send/!Sync). Surfaces
//! retain their runtime, and the runtime owns callback state until after free.
use eframe::egui::{self, Response, Widget};
use egui_term::{BackendSettings, PtyEvent, TerminalTheme};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{
    cell::RefCell,
    ffi::{CString, c_char, c_int, c_void},
    io::Write,
    ptr::NonNull,
    rc::{Rc, Weak},
    sync::mpsc::Sender,
};

unsafe extern "C" {
    fn relay_ghostty_new(
        parent: *mut c_void,
        config: *const c_char,
        data: *mut c_void,
        wake: extern "C" fn(*mut c_void),
    ) -> *mut c_void;
    fn relay_ghostty_free(runtime: *mut c_void);
    fn relay_ghostty_tick(runtime: *mut c_void);
    fn relay_ghostty_surface_new(
        runtime: *mut c_void,
        command: *const c_char,
        directory: *const c_char,
    ) -> *mut c_void;
    fn relay_ghostty_surface_free(surface: *mut c_void);
    fn relay_ghostty_surface_place(
        surface: *mut c_void,
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        focus: bool,
        release_focus: bool,
    );
    fn relay_ghostty_surface_hide(surface: *mut c_void);
    fn relay_ghostty_surface_exited(surface: *mut c_void, status: *mut c_int) -> bool;
}

thread_local! { static RUNTIME: RefCell<Weak<Inner>> = const { RefCell::new(Weak::new()) }; }

pub fn requested() -> bool {
    if std::env::args().any(|arg| arg == "--standard-terminal") {
        return false;
    }
    std::env::args().any(|arg| arg == "--ghostty" || arg == "--ghostty-demo")
        || std::env::var("RELAY_GHOSTTY").as_deref() == Ok("1")
        || bundled_resources().is_some()
}

fn bundle_resources_at(executable: &std::path::Path) -> Option<std::path::PathBuf> {
    let macos = executable.parent()?;
    if macos.file_name()? != "MacOS" {
        return None;
    }
    let resources = macos.parent()?.join("Resources/ghostty");
    resources
        .join("shell-integration")
        .is_dir()
        .then_some(resources)
}

fn bundled_resources() -> Option<std::path::PathBuf> {
    bundle_resources_at(&std::env::current_exe().ok()?)
}

pub fn resources_dir() -> std::path::PathBuf {
    bundled_resources().unwrap_or_else(|| env!("RELAY_GHOSTTY_RESOURCES").into())
}

/// Called at startup, before UI/worker threads exist.
pub fn prepare_environment() {
    if requested() {
        // SAFETY: call sites are process entry points before starting threads.
        unsafe {
            std::env::set_var("GHOSTTY_RESOURCES_DIR", resources_dir());
        }
    }
}

pub struct Runtime {
    _inner: Rc<Inner>,
}
struct Inner {
    handle: NonNull<c_void>,
    context: Box<egui::Context>,
}
impl Drop for Inner {
    fn drop(&mut self) {
        // All Surface instances have released their strong references first.
        unsafe {
            relay_ghostty_free(self.handle.as_ptr());
        }
    }
}
extern "C" fn wakeup(data: *mut c_void) {
    // Ghostty may call from a worker thread. Context::request_repaint is thread safe;
    // no native views or Rc state are touched here. Never unwind across the C ABI.
    let _ = std::panic::catch_unwind(|| unsafe {
        (&*data.cast::<egui::Context>()).request_repaint();
    });
}
impl Runtime {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Result<Self, String> {
        let RawWindowHandle::AppKit(window) =
            cc.window_handle().map_err(|e| e.to_string())?.as_raw()
        else {
            return Err("Ghostty requires a native macOS window".into());
        };
        let mut config = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
        // Deliberately use Relay's appearance rather than the user's standalone
        // Ghostty config. Profile authentication stays with the OpenSSH command.
        config
            .write_all(include_bytes!("assets/ghostty.conf"))
            .map_err(|e| e.to_string())?;
        let path = CString::new(config.path().as_os_str().as_encoded_bytes())
            .map_err(|e| e.to_string())?;
        let context = Box::new(cc.egui_ctx.clone());
        let handle = NonNull::new(unsafe {
            relay_ghostty_new(
                window.ns_view.as_ptr(),
                path.as_ptr(),
                (&*context as *const egui::Context).cast_mut().cast(),
                wakeup,
            )
        })
        .ok_or("Ghostty initialization failed; check prototype build/configuration")?;
        let inner = Rc::new(Inner { handle, context });
        RUNTIME.with(|runtime| *runtime.borrow_mut() = Rc::downgrade(&inner));
        Ok(Self { _inner: inner })
    }
}
pub fn tick() {
    if let Some(runtime) = RUNTIME.with(|r| r.borrow().upgrade()) {
        unsafe {
            relay_ghostty_tick(runtime.handle.as_ptr());
        }
    }
}

pub struct TerminalBackend {
    engine: Engine,
}
enum Engine {
    Standard(Box<egui_term::TerminalBackend>),
    Ghostty(Surface),
}
struct Surface {
    handle: NonNull<c_void>,
    runtime: Rc<Inner>,
    id: u64,
    events: Sender<(u64, PtyEvent)>,
    exit_sent: bool,
}
impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            relay_ghostty_surface_free(self.handle.as_ptr());
        }
    }
}

// The embedding API accepts a command string, unlike std::process::Command.
// Quote every argv element separately; profile fields must never become shell syntax.
fn command_line(settings: &BackendSettings) -> Result<CString, std::ffi::NulError> {
    CString::new(
        std::iter::once(&settings.shell)
            .chain(&settings.args)
            .map(|arg| format!("'{}'", arg.replace('\'', "'\\''")))
            .collect::<Vec<_>>()
            .join(" "),
    )
}

impl TerminalBackend {
    pub fn new(
        id: u64,
        ctx: egui::Context,
        events: Sender<(u64, PtyEvent)>,
        settings: BackendSettings,
    ) -> std::io::Result<Self> {
        if !requested() {
            return egui_term::TerminalBackend::new(id, ctx, events, settings).map(|backend| {
                Self {
                    engine: Engine::Standard(Box::new(backend)),
                }
            });
        }
        let runtime = RUNTIME
            .with(|r| r.borrow().upgrade())
            .ok_or_else(|| std::io::Error::other("Ghostty runtime is unavailable"))?;
        let command = command_line(&settings).map_err(std::io::Error::other)?;
        let directory = settings
            .working_directory
            .as_ref()
            .map(|d| CString::new(d.as_os_str().as_encoded_bytes()))
            .transpose()
            .map_err(std::io::Error::other)?;
        let handle = NonNull::new(unsafe {
            relay_ghostty_surface_new(
                runtime.handle.as_ptr(),
                command.as_ptr(),
                directory.as_ref().map_or(std::ptr::null(), |d| d.as_ptr()),
            )
        })
        .ok_or_else(|| std::io::Error::other("Ghostty could not create a terminal surface"))?;
        Ok(Self {
            engine: Engine::Ghostty(Surface {
                handle,
                runtime,
                id,
                events,
                exit_sent: false,
            }),
        })
    }

    pub fn pty_id(&self) -> Option<u32> {
        match &self.engine {
            Engine::Standard(b) => Some(b.pty_id()),
            Engine::Ghostty(_) => None,
        }
    }
    pub fn set_visible(&mut self, visible: bool) {
        match &mut self.engine {
            Engine::Standard(b) => b.set_visible(visible),
            Engine::Ghostty(surface) => {
                if !visible {
                    unsafe {
                        relay_ghostty_surface_hide(surface.handle.as_ptr());
                    }
                }
                let mut code = -1;
                if !surface.exit_sent
                    && unsafe { relay_ghostty_surface_exited(surface.handle.as_ptr(), &mut code) }
                {
                    use std::os::unix::process::ExitStatusExt;
                    let event = if (0..=255).contains(&code) {
                        PtyEvent::ChildExit(std::process::ExitStatus::from_raw(code << 8))
                    } else {
                        PtyEvent::Exit
                    };
                    let _ = surface.events.send((surface.id, event));
                    surface.exit_sent = true;
                    surface.runtime.context.request_repaint();
                }
            }
        }
    }

    // CPU benchmarks always use the existing deterministic, in-memory fixture.
    #[cfg(feature = "terminal-fixture")]
    #[allow(dead_code)]
    pub fn in_memory(id: u64) -> Self {
        Self {
            engine: Engine::Standard(Box::new(egui_term::TerminalBackend::in_memory(id))),
        }
    }
    #[cfg(feature = "terminal-fixture")]
    #[allow(dead_code)]
    pub fn feed_output(&mut self, bytes: &[u8]) {
        let Engine::Standard(backend) = &mut self.engine else {
            panic!("in-memory fixture only")
        };
        backend.feed_output(bytes);
    }
    #[cfg(feature = "terminal-fixture")]
    #[allow(dead_code)]
    pub fn process_command(&mut self, command: egui_term::BackendCommand) {
        let Engine::Standard(backend) = &mut self.engine else {
            panic!("in-memory fixture only")
        };
        backend.process_command(command);
    }
    #[cfg(all(test, feature = "terminal-fixture"))]
    pub fn fixture_text(&mut self) -> String {
        let Engine::Standard(backend) = &mut self.engine else {
            panic!("in-memory fixture only")
        };
        backend.sync().cells.iter().map(|c| c.c).collect()
    }
}

pub struct TerminalView<'a> {
    backend: &'a mut TerminalBackend,
    theme: Option<TerminalTheme>,
    focus: bool,
}
impl<'a> TerminalView<'a> {
    pub fn new(_ui: &egui::Ui, backend: &'a mut TerminalBackend) -> Self {
        Self {
            backend,
            theme: None,
            focus: false,
        }
    }
    pub fn set_theme(mut self, theme: TerminalTheme) -> Self {
        self.theme = Some(theme);
        self
    }
    pub fn set_focus(mut self, focus: bool) -> Self {
        self.focus = focus;
        self
    }
}
impl Widget for TerminalView<'_> {
    fn ui(self, ui: &mut egui::Ui) -> Response {
        match &mut self.backend.engine {
            Engine::Standard(backend) => {
                let mut view = egui_term::TerminalView::new(ui, backend).set_focus(self.focus);
                if let Some(theme) = self.theme {
                    view = view.set_theme(theme);
                }
                ui.add(view)
            }
            Engine::Ghostty(surface) => {
                let (rect, response) =
                    ui.allocate_exact_size(ui.available_size(), egui::Sense::hover());
                if ui.is_enabled() {
                    // egui points include application zoom; AppKit uses native points.
                    let zoom = f64::from(ui.ctx().zoom_factor());
                    // winit's parent view receives clicks outside this native
                    // subview, but does not restore itself as first responder.
                    let release_focus = ui.input(|i| {
                        i.pointer.any_pressed()
                            && i.pointer.interact_pos().is_some_and(|p| !rect.contains(p))
                    });
                    unsafe {
                        relay_ghostty_surface_place(
                            surface.handle.as_ptr(),
                            f64::from(rect.min.x) * zoom,
                            f64::from(rect.min.y) * zoom,
                            f64::from(rect.width()) * zoom,
                            f64::from(rect.height()) * zoom,
                            self.focus,
                            release_focus,
                        );
                    }
                } else {
                    // Native subviews sit above egui. Hide them while its modal is
                    // open so the terminal cannot obscure or intercept the dialog.
                    unsafe {
                        relay_ghostty_surface_hide(surface.handle.as_ptr());
                    }
                }
                response
            }
        }
    }
}

#[cfg(feature = "terminal-fixture")]
#[allow(dead_code)]
impl TerminalBackend {
    fn probe_handle(&self) -> *mut c_void {
        let Engine::Ghostty(surface) = &self.engine else {
            panic!("native Ghostty fixture required")
        };
        surface.handle.as_ptr()
    }
    pub fn probe_text(&self) -> String {
        unsafe extern "C" {
            fn relay_ghostty_probe_text(
                surface: *mut c_void,
                output: *mut u8,
                capacity: usize,
            ) -> usize;
        }
        let mut buffer = vec![0; 128 * 1024];
        let count = unsafe {
            relay_ghostty_probe_text(self.probe_handle(), buffer.as_mut_ptr(), buffer.len())
        };
        String::from_utf8_lossy(&buffer[..count]).into_owned()
    }
    pub fn probe_key(&self, text: &str, keycode: u16, control: bool) {
        unsafe extern "C" {
            fn relay_ghostty_probe_key(
                surface: *mut c_void,
                text: *const c_char,
                keycode: u16,
                control: bool,
            );
        }
        let text = CString::new(text).unwrap();
        unsafe {
            relay_ghostty_probe_key(self.probe_handle(), text.as_ptr(), keycode, control);
        }
    }
    pub fn probe_clipboard(&self, expected: &str, paste: Option<&str>) -> bool {
        unsafe extern "C" {
            fn relay_ghostty_probe_clipboard(
                surface: *mut c_void,
                expected: *const c_char,
                paste: *const c_char,
            ) -> bool;
        }
        let expected = CString::new(expected).unwrap();
        let paste = paste.map(|text| CString::new(text).unwrap());
        unsafe {
            relay_ghostty_probe_clipboard(
                self.probe_handle(),
                expected.as_ptr(),
                paste
                    .as_ref()
                    .map_or(std::ptr::null(), |text| text.as_ptr()),
            )
        }
    }
    pub fn probe_visible(&self) -> bool {
        unsafe extern "C" {
            fn relay_ghostty_probe_visible(surface: *mut c_void) -> bool;
        }
        unsafe { relay_ghostty_probe_visible(self.probe_handle()) }
    }
    pub fn probe_focused(&self) -> bool {
        unsafe extern "C" {
            fn relay_ghostty_probe_focused(surface: *mut c_void) -> bool;
        }
        unsafe { relay_ghostty_probe_focused(self.probe_handle()) }
    }
    pub fn probe_columns(&self) -> u16 {
        unsafe extern "C" {
            fn relay_ghostty_probe_columns(surface: *mut c_void) -> u16;
        }
        unsafe { relay_ghostty_probe_columns(self.probe_handle()) }
    }
    pub fn probe_ink_pixels(&self) -> usize {
        unsafe extern "C" {
            fn relay_ghostty_probe_ink_pixels(surface: *mut c_void) -> usize;
        }
        unsafe { relay_ghostty_probe_ink_pixels(self.probe_handle()) }
    }
    pub fn probe_scroll(&self, fraction_x: f64, lines: f64) {
        unsafe extern "C" {
            fn relay_ghostty_probe_scroll(surface: *mut c_void, fraction_x: f64, lines: f64);
        }
        unsafe {
            relay_ghostty_probe_scroll(self.probe_handle(), fraction_x, lines);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn packaged_resources_follow_the_executable_location() {
        let temp = tempfile::tempdir().unwrap();
        let contents = temp.path().join("Moved Relay.app/Contents");
        let executable = contents.join("MacOS/relay");
        assert!(super::bundle_resources_at(&executable).is_none());
        std::fs::create_dir_all(contents.join("Resources/ghostty/shell-integration")).unwrap();
        assert_eq!(
            super::bundle_resources_at(&executable),
            Some(contents.join("Resources/ghostty"))
        );
        assert!(super::bundle_resources_at(&temp.path().join("bin/relay")).is_none());
    }
    use super::*;
    #[test]
    fn command_arguments_roundtrip_without_shell_expansion() {
        let values = [
            "hello world",
            "a'b",
            "$(echo unexpected)",
            "`echo unexpected`",
            "a; echo unexpected",
            "",
            "日本語",
        ];
        let settings = BackendSettings {
            shell: "/usr/bin/printf".into(),
            args: std::iter::once("%s\\0".into())
                .chain(values.iter().map(|s| s.to_string()))
                .collect(),
            working_directory: None,
        };
        let output = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(command_line(&settings).unwrap().to_str().unwrap())
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(
            output.stdout,
            values
                .iter()
                .flat_map(|s| s.bytes().chain([0]))
                .collect::<Vec<_>>()
        );
    }
}
