//! Native interactive window aspect-ratio constraints.
//!
//! Wayland's xdg_toplevel protocol has no aspect-ratio hint; on Wayland the
//! application therefore preserves the rendered content ratio with bars.

use sdl2::video::Window;

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use std::ffi::{c_char, c_void, CString};

    static ASPECT_STATE_KEY: u8 = 0;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Size {
        width: f64,
        height: f64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Point {
        x: f64,
        y: f64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Rect {
        origin: Point,
        size: Size,
    }

    struct State {
        sdl_window: *mut sdl2_sys::SDL_Window,
        ratio: Size,
        enabled: bool,
        resize_axis: Option<ResizeAxis>,
    }

    #[derive(Clone, Copy)]
    enum ResizeAxis {
        Width,
        Height,
    }

    #[link(name = "objc")]
    unsafe extern "C" {
        fn sel_registerName(name: *const c_char) -> *mut c_void;
        fn objc_msgSend();
        #[cfg(target_arch = "x86_64")]
        fn objc_msgSend_stret();
        fn object_getClass(object: *mut c_void) -> *mut c_void;
        fn class_addMethod(
            class: *mut c_void,
            selector: *mut c_void,
            implementation: *const c_void,
            types: *const c_char,
        ) -> bool;
        fn class_getInstanceMethod(class: *mut c_void, selector: *mut c_void) -> *mut c_void;
        fn method_getImplementation(method: *mut c_void) -> *const c_void;
        fn objc_getAssociatedObject(object: *mut c_void, key: *const c_void) -> *mut c_void;
        fn objc_setAssociatedObject(
            object: *mut c_void,
            key: *const c_void,
            value: *mut c_void,
            policy: usize,
        );
    }

    unsafe fn send_object(object: *mut c_void, name: &str) -> *mut c_void {
        let selector = CString::new(name).unwrap();
        let send: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
            unsafe { std::mem::transmute(objc_msgSend as *const ()) };
        unsafe { send(object, sel_registerName(selector.as_ptr())) }
    }

    unsafe fn install_method(
        class: *mut c_void,
        name: &std::ffi::CStr,
        implementation: *const c_void,
        types: &std::ffi::CStr,
    ) -> bool {
        if class.is_null() {
            return false;
        }
        let selector = unsafe { sel_registerName(name.as_ptr()) };
        if unsafe { class_addMethod(class, selector, implementation, types.as_ptr()) } {
            true
        } else {
            let method = unsafe { class_getInstanceMethod(class, selector) };
            !method.is_null() && unsafe { method_getImplementation(method) == implementation }
        }
    }

    unsafe extern "C" fn live_resize_boundary(
        _delegate: *mut c_void,
        _selector: *mut c_void,
        notification: *mut c_void,
    ) {
        let window = unsafe { send_object(notification, "object") };
        let state = unsafe {
            objc_getAssociatedObject(window, &ASPECT_STATE_KEY as *const u8 as *const c_void)
        } as *mut State;
        if !state.is_null() {
            unsafe { (*state).resize_axis = None };
        }
    }

    unsafe fn content_rect(window: *mut c_void, frame: Rect) -> Rect {
        let selector = c"contentRectForFrameRect:";
        let selector = unsafe { sel_registerName(selector.as_ptr()) };
        #[cfg(target_arch = "aarch64")]
        {
            let send: unsafe extern "C" fn(*mut c_void, *mut c_void, Rect) -> Rect =
                unsafe { std::mem::transmute(objc_msgSend as *const ()) };
            unsafe { send(window, selector, frame) }
        }
        #[cfg(target_arch = "x86_64")]
        {
            let mut result = std::mem::MaybeUninit::<Rect>::uninit();
            let send: unsafe extern "C" fn(*mut Rect, *mut c_void, *mut c_void, Rect) =
                unsafe { std::mem::transmute(objc_msgSend_stret as *const ()) };
            unsafe {
                send(result.as_mut_ptr(), window, selector, frame);
                result.assume_init()
            }
        }
    }

    unsafe extern "C" fn window_will_resize(
        _delegate: *mut c_void,
        _selector: *mut c_void,
        window: *mut c_void,
        proposed: Size,
    ) -> Size {
        let state = unsafe {
            objc_getAssociatedObject(window, &ASPECT_STATE_KEY as *const u8 as *const c_void)
        } as *const State;
        if state.is_null() || !unsafe { (*state).enabled } {
            return proposed;
        }
        let ratio = unsafe { (*state).ratio };
        let frame = Rect {
            origin: Point { x: 0.0, y: 0.0 },
            size: proposed,
        };
        let content = unsafe { content_rect(window, frame) }.size;
        if content.width <= 0.0 || content.height <= 0.0 {
            return proposed;
        }

        let mut current_width = 0;
        let mut current_height = 0;
        unsafe {
            sdl2_sys::SDL_GetWindowSize(
                (*state).sdl_window,
                &mut current_width,
                &mut current_height,
            );
        }
        let state = unsafe { &mut *state.cast_mut() };
        if state.resize_axis.is_none()
            && (content.width - f64::from(current_width)).abs() < 0.5
            && (content.height - f64::from(current_height)).abs() < 0.5
        {
            return proposed;
        }
        let axis = *state.resize_axis.get_or_insert_with(|| {
            let width_changed = (content.width - f64::from(current_width)).abs() / ratio.width;
            let height_changed = (content.height - f64::from(current_height)).abs() / ratio.height;
            if width_changed >= height_changed {
                ResizeAxis::Width
            } else {
                ResizeAxis::Height
            }
        });
        let (width, height) = match axis {
            ResizeAxis::Width => (content.width, content.width * ratio.height / ratio.width),
            ResizeAxis::Height => (content.height * ratio.width / ratio.height, content.height),
        };
        Size {
            width: width + proposed.width - content.width,
            height: height + proposed.height - content.height,
        }
    }

    pub struct Constraint {
        window: *mut c_void,
        state: *mut State,
    }

    impl Constraint {
        pub fn new(window: &Window) -> Self {
            let mut info: sdl2_sys::SDL_SysWMinfo = unsafe { std::mem::zeroed() };
            unsafe {
                sdl2_sys::SDL_GetVersion(&mut info.version);
            }
            let ok = unsafe { sdl2_sys::SDL_GetWindowWMInfo(window.raw(), &mut info) }
                == sdl2_sys::SDL_bool::SDL_TRUE;
            let native = if ok && info.subsystem == sdl2_sys::SDL_SYSWM_TYPE::SDL_SYSWM_COCOA {
                unsafe { *(info.info.dummy.as_ptr() as *const *mut c_void) }
            } else {
                std::ptr::null_mut()
            };
            if native.is_null() {
                return Self {
                    window: native,
                    state: std::ptr::null_mut(),
                };
            }
            let delegate = unsafe { send_object(native, "delegate") };
            let class = unsafe { object_getClass(delegate) };
            let installed = unsafe {
                install_method(
                    class,
                    c"windowWillResize:toSize:",
                    window_will_resize as *const c_void,
                    c"{CGSize=dd}@:@{CGSize=dd}",
                ) && install_method(
                    class,
                    c"windowWillStartLiveResize:",
                    live_resize_boundary as *const c_void,
                    c"v@:@",
                ) && install_method(
                    class,
                    c"windowDidEndLiveResize:",
                    live_resize_boundary as *const c_void,
                    c"v@:@",
                )
            };
            let state = if installed {
                let state = Box::into_raw(Box::new(State {
                    sdl_window: window.raw(),
                    ratio: Size {
                        width: 1.0,
                        height: 1.0,
                    },
                    enabled: false,
                    resize_axis: None,
                }));
                unsafe {
                    objc_setAssociatedObject(
                        native,
                        &ASPECT_STATE_KEY as *const u8 as *const c_void,
                        state.cast(),
                        0, // OBJC_ASSOCIATION_ASSIGN
                    );
                }
                state
            } else {
                std::ptr::null_mut()
            };
            Self {
                window: native,
                state,
            }
        }

        pub fn set(&mut self, enabled: bool, ratio: (u32, u32)) {
            if self.state.is_null() {
                return;
            }
            unsafe {
                (*self.state).enabled = enabled && ratio.0 > 0 && ratio.1 > 0;
                (*self.state).resize_axis = None;
                (*self.state).ratio = Size {
                    width: ratio.0 as f64,
                    height: ratio.1 as f64,
                };
            }
        }

        pub fn is_native(&self) -> bool {
            !self.state.is_null()
        }
    }

    impl Drop for Constraint {
        fn drop(&mut self) {
            if !self.state.is_null() {
                unsafe {
                    objc_setAssociatedObject(
                        self.window,
                        &ASPECT_STATE_KEY as *const u8 as *const c_void,
                        std::ptr::null_mut(),
                        0,
                    );
                    drop(Box::from_raw(self.state));
                }
            }
        }
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
    use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetClientRect, GetWindowRect, WM_SIZING};

    const WMSZ_LEFT: usize = 1;
    const WMSZ_RIGHT: usize = 2;
    const WMSZ_TOP: usize = 3;
    const WMSZ_BOTTOM: usize = 6;
    const WMSZ_TOPLEFT: usize = 4;
    const WMSZ_TOPRIGHT: usize = 5;
    const WMSZ_BOTTOMLEFT: usize = 7;
    const WMSZ_BOTTOMRIGHT: usize = 8;
    const SUBCLASS_ID: usize = 0x5448_4c45;

    struct State {
        enabled: AtomicBool,
        width: AtomicU32,
        height: AtomicU32,
    }
    pub struct Constraint {
        hwnd: HWND,
        state: *mut State,
        native: bool,
    }

    unsafe extern "system" fn resize(
        hwnd: HWND,
        msg: u32,
        edge: WPARAM,
        data: LPARAM,
        _id: usize,
        state_ptr: usize,
    ) -> LRESULT {
        if msg != WM_SIZING {
            return unsafe { DefSubclassProc(hwnd, msg, edge, data) };
        }
        if state_ptr == 0 {
            return unsafe { DefSubclassProc(hwnd, msg, edge, data) };
        }
        let state = unsafe { &*(state_ptr as *const State) };
        let width = state.width.load(Ordering::Relaxed);
        let height = state.height.load(Ordering::Relaxed);
        if !state.enabled.load(Ordering::Relaxed) || data == 0 || width == 0 || height == 0 {
            return unsafe { DefSubclassProc(hwnd, msg, edge, data) };
        }
        let rect = unsafe { &mut *(data as *mut RECT) };
        let mut outer: RECT = unsafe { std::mem::zeroed() };
        let mut client: RECT = unsafe { std::mem::zeroed() };
        if unsafe { GetWindowRect(hwnd, &mut outer) } == 0
            || unsafe { GetClientRect(hwnd, &mut client) } == 0
        {
            return unsafe { DefSubclassProc(hwnd, msg, edge, data) };
        }
        let frame_w = (outer.right - outer.left) - (client.right - client.left);
        let frame_h = (outer.bottom - outer.top) - (client.bottom - client.top);
        let w = ((rect.right - rect.left) - frame_w).max(1) as i64;
        let h = ((rect.bottom - rect.top) - frame_h).max(1) as i64;
        let ratio_w = width as i64;
        let ratio_h = height as i64;
        let horizontal_edge = matches!(edge, WMSZ_LEFT | WMSZ_RIGHT);
        let vertical_edge = matches!(edge, WMSZ_TOP | WMSZ_BOTTOM);
        let height_from_width = (w * ratio_h + ratio_w / 2) / ratio_w;
        let width_from_height = (h * ratio_w + ratio_h / 2) / ratio_h;
        let adjust_height = horizontal_edge
            || (!vertical_edge && (height_from_width - h).abs() <= (width_from_height - w).abs());
        let (new_w, new_h) = if adjust_height {
            (w, (w * ratio_h + ratio_w / 2) / ratio_w)
        } else {
            ((h * ratio_w + ratio_h / 2) / ratio_h, h)
        };
        let (new_w, new_h) = (new_w as i32 + frame_w, new_h as i32 + frame_h);
        match edge {
            WMSZ_LEFT | WMSZ_TOPLEFT | WMSZ_BOTTOMLEFT => rect.left = rect.right - new_w,
            _ => rect.right = rect.left + new_w,
        }
        match edge {
            WMSZ_TOP | WMSZ_TOPLEFT | WMSZ_TOPRIGHT => rect.top = rect.bottom - new_h,
            WMSZ_BOTTOM | WMSZ_BOTTOMLEFT | WMSZ_BOTTOMRIGHT => rect.bottom = rect.top + new_h,
            _ => {
                let center = (rect.top + rect.bottom) / 2;
                rect.top = center - new_h / 2;
                rect.bottom = rect.top + new_h;
            }
        }
        1 // WM_SIZING: handled; the adjusted drag RECT is applied by Windows.
    }

    impl Constraint {
        pub fn new(window: &Window) -> Self {
            let hwnd = unsafe {
                let mut info: sdl2_sys::SDL_SysWMinfo = std::mem::zeroed();
                sdl2_sys::SDL_GetVersion(&mut info.version);
                if sdl2_sys::SDL_GetWindowWMInfo(window.raw(), &mut info)
                    == sdl2_sys::SDL_bool::SDL_TRUE
                    && info.subsystem == sdl2_sys::SDL_SYSWM_TYPE::SDL_SYSWM_WINDOWS
                {
                    *(info.info.dummy.as_ptr() as *const HWND)
                } else {
                    std::ptr::null_mut()
                }
            };
            let mut state = if hwnd.is_null() {
                std::ptr::null_mut()
            } else {
                Box::into_raw(Box::new(State {
                    enabled: AtomicBool::new(false),
                    width: AtomicU32::new(1),
                    height: AtomicU32::new(1),
                }))
            };
            let native = if !state.is_null() {
                if unsafe {
                    SetWindowSubclass(hwnd, Some(resize), SUBCLASS_ID, state as usize) != 0
                } {
                    true
                } else {
                    unsafe { drop(Box::from_raw(state)) };
                    state = std::ptr::null_mut();
                    false
                }
            } else {
                false
            };
            Self {
                hwnd,
                state,
                native,
            }
        }
        pub fn set(&mut self, enabled: bool, ratio: (u32, u32)) {
            if self.state.is_null() {
                return;
            }
            let state = unsafe { &*self.state };
            if ratio.0 > 0 && ratio.1 > 0 {
                state.width.store(ratio.0, Ordering::Relaxed);
                state.height.store(ratio.1, Ordering::Relaxed);
            }
            state.enabled.store(enabled, Ordering::Relaxed);
        }
        pub fn is_native(&self) -> bool {
            self.native
        }
    }
    impl Drop for Constraint {
        fn drop(&mut self) {
            if !self.state.is_null() {
                if self.native
                    && unsafe { RemoveWindowSubclass(self.hwnd, Some(resize), SUBCLASS_ID) != 0 }
                {
                    unsafe { drop(Box::from_raw(self.state)) };
                }
                // If removal fails, the callback may still hold this pointer.
                // Leak the tiny state rather than risk a use-after-free.
            }
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;
    use std::ffi::{c_int, c_void};

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Ratio {
        x: c_int,
        y: c_int,
    }
    #[repr(C)]
    #[derive(Default)]
    struct SizeHints {
        flags: std::ffi::c_long,
        x: c_int,
        y: c_int,
        width: c_int,
        height: c_int,
        min_width: c_int,
        min_height: c_int,
        max_width: c_int,
        max_height: c_int,
        width_inc: c_int,
        height_inc: c_int,
        min_aspect: Ratio,
        max_aspect: Ratio,
        base_width: c_int,
        base_height: c_int,
        win_gravity: c_int,
    }
    type GetNormalHints = unsafe extern "C" fn(
        display: *mut sdl2_sys::Display,
        window: sdl2_sys::Window,
        hints: *mut SizeHints,
        supplied: *mut std::ffi::c_long,
    ) -> c_int;
    type SetNormalHints = unsafe extern "C" fn(
        display: *mut sdl2_sys::Display,
        window: sdl2_sys::Window,
        hints: *mut SizeHints,
    ) -> c_int;
    type Flush = unsafe extern "C" fn(display: *mut sdl2_sys::Display) -> c_int;

    struct X11Api {
        handle: *mut c_void,
        get_normal_hints: GetNormalHints,
        set_normal_hints: SetNormalHints,
        flush: Flush,
    }
    impl X11Api {
        fn load() -> Option<Self> {
            for library in [b"libX11.so.6\0".as_slice(), b"libX11.so\0".as_slice()] {
                let handle = unsafe {
                    libc::dlopen(library.as_ptr().cast(), libc::RTLD_LAZY | libc::RTLD_LOCAL)
                };
                if handle.is_null() {
                    continue;
                }
                let get = unsafe { libc::dlsym(handle, b"XGetWMNormalHints\0".as_ptr().cast()) };
                let set = unsafe { libc::dlsym(handle, b"XSetWMNormalHints\0".as_ptr().cast()) };
                let flush = unsafe { libc::dlsym(handle, b"XFlush\0".as_ptr().cast()) };
                if get.is_null() || set.is_null() || flush.is_null() {
                    unsafe { libc::dlclose(handle) };
                    continue;
                }
                return Some(Self {
                    handle,
                    get_normal_hints: unsafe { std::mem::transmute(get) },
                    set_normal_hints: unsafe { std::mem::transmute(set) },
                    flush: unsafe { std::mem::transmute(flush) },
                });
            }
            None
        }
    }
    impl Drop for X11Api {
        fn drop(&mut self) {
            unsafe { libc::dlclose(self.handle) };
        }
    }
    pub struct Constraint {
        api: Option<X11Api>,
        display: *mut sdl2_sys::Display,
        window: sdl2_sys::Window,
        saved_base: Option<(bool, c_int, c_int)>,
    }
    impl Constraint {
        pub fn new(window: &Window) -> Self {
            let mut info: sdl2_sys::SDL_SysWMinfo = unsafe { std::mem::zeroed() };
            unsafe {
                sdl2_sys::SDL_GetVersion(&mut info.version);
            }
            let valid = unsafe { sdl2_sys::SDL_GetWindowWMInfo(window.raw(), &mut info) }
                == sdl2_sys::SDL_bool::SDL_TRUE
                && info.subsystem == sdl2_sys::SDL_SYSWM_TYPE::SDL_SYSWM_X11;
            if valid {
                let x11 = unsafe { info.info.x11 };
                Self {
                    api: X11Api::load(),
                    display: x11.display,
                    window: x11.window,
                    saved_base: None,
                }
            } else {
                Self {
                    api: None,
                    display: std::ptr::null_mut(),
                    window: 0,
                    saved_base: None,
                }
            }
        }
        pub fn set(&mut self, enabled: bool, ratio: (u32, u32)) {
            let Some(api) = self.api.as_ref() else {
                return;
            };
            if self.display.is_null() || self.window == 0 {
                return;
            }
            let mut hints = SizeHints::default();
            let mut supplied = 0;
            unsafe {
                if (api.get_normal_hints)(self.display, self.window, &mut hints, &mut supplied) == 0
                {
                    return;
                }
                if enabled && ratio.0 > 0 && ratio.1 > 0 {
                    if self.saved_base.is_none() {
                        self.saved_base = Some((
                            hints.flags & (1 << 8) != 0,
                            hints.base_width,
                            hints.base_height,
                        ));
                    }
                    hints.flags |= 1 << 7; // PAspect
                    hints.flags |= 1 << 8; // PBaseSize
                    hints.base_width = 0;
                    hints.base_height = 0;
                    hints.min_aspect = Ratio {
                        x: ratio.0 as c_int,
                        y: ratio.1 as c_int,
                    };
                    hints.max_aspect = hints.min_aspect;
                } else if let Some((had_base_size, base_width, base_height)) =
                    self.saved_base.take()
                {
                    hints.flags &= !(1 << 7);
                    if had_base_size {
                        hints.flags |= 1 << 8;
                        hints.base_width = base_width;
                        hints.base_height = base_height;
                    } else {
                        hints.flags &= !(1 << 8);
                        hints.base_width = 0;
                        hints.base_height = 0;
                    }
                }
                if (api.set_normal_hints)(self.display, self.window, &mut hints) != 0 {
                    (api.flush)(self.display);
                }
            }
        }
        pub fn is_native(&self) -> bool {
            self.api.is_some() && !self.display.is_null() && self.window != 0
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
mod platform {
    use super::*;
    pub struct Constraint;
    impl Constraint {
        pub fn new(_: &Window) -> Self {
            Self
        }
        pub fn set(&mut self, _: bool, _: (u32, u32)) {}
        pub fn is_native(&self) -> bool {
            false
        }
    }
}

pub use platform::Constraint;
