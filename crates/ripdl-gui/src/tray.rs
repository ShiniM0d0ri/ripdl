//! System tray icon + context menu (`Show ripdl` / `Quit`), fail-open on
//! headless or unsupported desktop sessions.

use std::sync::mpsc;

use eframe::egui;
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    Show,
    Quit,
}

pub struct TrayHandle {
    _tray: TrayIcon,
}

pub fn load_icon_rgba() -> Option<(Vec<u8>, u32, u32)> {
    let img = image::load_from_memory(include_bytes!("../icons/icon.png"))
        .ok()?
        .into_rgba8();
    let (w, h) = img.dimensions();
    Some((img.into_raw(), w, h))
}

pub fn init(tx: mpsc::Sender<TrayAction>, ctx: egui::Context) -> Option<TrayHandle> {
    let (rgba, w, h) = load_icon_rgba()?;
    let icon = Icon::from_rgba(rgba, w, h).ok()?;

    let menu = Menu::new();
    let show_item = MenuItem::new("Show ripdl", true, None);
    let quit_item = MenuItem::new("Quit", true, None);
    let show_id = show_item.id().clone();
    let quit_id = quit_item.id().clone();
    menu.append(&show_item).ok()?;
    menu.append(&quit_item).ok()?;

    let click_tx = tx.clone();
    let click_ctx = ctx.clone();
    TrayIconEvent::set_event_handler(Some(move |ev: TrayIconEvent| match ev {
        TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        }
        | TrayIconEvent::DoubleClick {
            button: MouseButton::Left,
            ..
        } => {
            let _ = click_tx.send(TrayAction::Show);
            click_ctx.request_repaint();
        }
        _ => {}
    }));

    MenuEvent::set_event_handler(Some(move |ev: MenuEvent| {
        if ev.id == show_id {
            let _ = tx.send(TrayAction::Show);
            ctx.request_repaint();
        } else if ev.id == quit_id {
            let _ = tx.send(TrayAction::Quit);
            ctx.request_repaint();
        }
    }));

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("ripdl")
        .with_icon(icon)
        .with_menu_on_left_click(false)
        .build()
        .ok()?;

    Some(TrayHandle { _tray: tray })
}
