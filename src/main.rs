//! fiaboo-shell: a GPUI layer-shell bar (bottom of the screen) with Start, workspaces, running
//! apps (icons, click to focus), tray, status icons and clock, plus one module per panel.
//! Colours come from `palette()` (palette.conf written by `theme`, Catppuccin Mocha fallback).
use crate::bar::{Bar, open_bar};
use crate::panels::osd::start_volume_watch;
use crate::panels::quick::restore_keep_awake;
use crate::services::hypr::{current_layout, start_hypr_events};
use crate::services::ipc::{send_ipc, start_ipc};
use crate::services::system::start_indicators;
use gpui::App;
use gpui_platform::application;
use std::time::Duration;
mod bar;
mod commons;
mod panels;
mod services;
#[cfg(test)]
mod tests;

use crate::bar::musicline;
use crate::services::{notify, tray};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args == ["layout"] {
        // asks Hyprland itself, so it answers with or without a running shell
        println!("{}", current_layout());
        std::process::exit(0);
    }
    if !args.is_empty() {
        send_ipc(&args.join(" "));
    }
    start_ipc();
    notify::start();
    tray::start();
    start_hypr_events();
    start_volume_watch();
    start_indicators();
    // Explicit: losing the bar (its only window) must not quit the app before on_window_closed reopens it.
    application().with_quit_mode(gpui::QuitMode::Explicit).run(|cx: &mut App| {
        gpui_base::init(cx); // the text input's key bindings and the theme global it reads; no Root wrapper needed
        // gpui-base snaps scrollbars in and out; the shell's motion is soft and slow
        gpui_base::Theme::global_mut(cx).scrollbar = gpui_base::ScrollbarTheme::new().with_motion(
            gpui_base::ScrollbarMotion::default()
                .with_enter(Duration::from_millis(300))
                .with_exit(Duration::from_millis(700))
                .with_expand(Duration::from_millis(300)),
        );
        open_bar(cx);
        // Hyprland closes every layer surface when its monitor goes away (DP-1 re-plugs itself at login);
        // GPUI then just drops the window, so put the bar back once a monitor is there again.
        cx.on_window_closed(|cx, _| {
            if cx.windows().iter().any(|w| w.downcast::<Bar>().is_some()) {
                return;
            }
            cx.spawn(async move |cx| {
                cx.background_executor().timer(Duration::from_millis(500)).await;
                let _ = cx.update(|cx| {
                    if cx.windows().iter().all(|w| w.downcast::<Bar>().is_none()) {
                        open_bar(cx);
                    }
                });
            })
            .detach();
        })
        .detach();
        musicline::restore(cx);
        restore_keep_awake();
    });
}
