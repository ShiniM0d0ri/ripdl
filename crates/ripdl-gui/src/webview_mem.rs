//! Free the WebView2's memory while ripdl sits in the tray.
//!
//! WebView2 has no idea the window belongs to a tray app: hiding the window
//! leaves the renderer, the GPU process and the compositor surfaces resident
//! (measured on this machine: ~192 MB private with the window shown *and* with
//! it hidden to the tray - hiding alone frees nothing).
//!
//! Suspending the webview drops those surfaces and freezes the page's timers,
//! which also parks the frontend's poll while nothing is on screen. `Resume`
//! brings the last frame back and the pending poll fires immediately, so the
//! list is already fresh when the window reappears.
//!
//! Tauri only flips the *window's* visibility when it hides one, never the
//! webview's, so this module sets the controller's visible flag itself;
//! WebView2 refuses to suspend a webview it still considers visible.
//!
//! The controller and its CoreWebView2 are apartment-threaded: they may only be
//! touched on the UI thread. `WebviewWindow::with_webview` queues a closure onto
//! that thread, so calling these from the tray or event handlers is safe.
//!
//! Only Windows has this lever; elsewhere the functions are no-ops and hiding
//! to the tray behaves exactly as before.

/// Suspend the webview. Call once the window is hidden.
pub fn suspend(window: &tauri::WebviewWindow) {
    #[cfg(windows)]
    {
        let _ = window.with_webview(|platform| {
            // SAFETY: runs on the UI thread (with_webview guarantees it), the
            // thread that created the controller.
            unsafe {
                let controller: webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Controller =
                    platform.controller();
                // A failed call is harmless: the window is hidden either way
                // and the next reveal() flips the flag back.
                if controller.SetIsVisible(false).is_err() {
                    return;
                }
                let Ok(core) = controller.CoreWebView2() else {
                    return;
                };
                // Low memory target lets Chromium drop caches and its
                // back/forward store while nothing is on screen.
                if let Ok(v19) = windows_core::Interface::cast::<
                    webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2_19,
                >(&core)
                {
                    let _ = v19.SetMemoryUsageTargetLevel(
                        webview2_com::Microsoft::Web::WebView2::Win32::COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW,
                    );
                }
                if let Ok(webview) = windows_core::Interface::cast::<
                    webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2_3,
                >(&core)
                {
                    let done = webview2_com::TrySuspendCompletedHandler::create(Box::new(|_, _| Ok(())));
                    let _ = webview.TrySuspend(&done);
                }
            }
        });
    }
    #[cfg(not(windows))]
    let _ = window;
}

/// Undo `suspend`. Harmless when the webview was never suspended; call it
/// before showing the window so the first frame is already live.
pub fn resume(window: &tauri::WebviewWindow) {
    #[cfg(windows)]
    {
        let _ = window.with_webview(|platform| {
            // SAFETY: see `suspend`.
            unsafe {
                let controller: webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Controller =
                    platform.controller();
                if let Ok(core) = controller.CoreWebView2() {
                    if let Ok(webview) = windows_core::Interface::cast::<
                        webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2_3,
                    >(&core)
                    {
                        let _ = webview.Resume();
                    }
                    if let Ok(v19) = windows_core::Interface::cast::<
                        webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2_19,
                    >(&core)
                    {
                        let _ = v19.SetMemoryUsageTargetLevel(
                            webview2_com::Microsoft::Web::WebView2::Win32::COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL,
                        );
                    }
                }
                let _ = controller.SetIsVisible(true);
            }
        });
    }
    #[cfg(not(windows))]
    let _ = window;
}
