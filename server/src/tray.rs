//! The optional desktop GUI: a tray icon (Windows notification area, macOS menu
//! bar) with a small menu. The real interface stays the admin page, so there is
//! only one place to learn; the tray makes the server feel like a normal app.

use crate::{App, open_url};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIconBuilder};

pub fn run(app: Arc<App>) {
    let event_loop = EventLoopBuilder::new().build();
    let open = MenuItem::new("Open admin page", true, None);
    let folder = MenuItem::new("Open data folder", true, None);
    let quit = MenuItem::new("Stop server", true, None);
    let menu = Menu::new();
    menu.append_items(&[&open, &folder, &PredefinedMenuItem::separator(), &quit]).expect("tray menu");
    let mut tray = None;
    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(250));
        if let Event::NewEvents(StartCause::Init) = event {
            // Created inside the loop: macOS needs the app to be running first.
            tray = TrayIconBuilder::new()
                .with_menu(Box::new(menu.clone()))
                .with_tooltip(format!("Evelopment server – {}", app.db.get_or("server_name", "")))
                .with_icon(icon())
                .build()
                .map_err(|e| eprintln!("No tray icon: {e}"))
                .ok();
        }
        while let Ok(e) = MenuEvent::receiver().try_recv() {
            if e.id == open.id() {
                open_url(&app.admin_url());
            } else if e.id == folder.id() {
                open_url(&app.data_dir.to_string_lossy());
            } else if e.id == quit.id() {
                app.hub.save_all(&app);
                tray.take();
                std::process::exit(0);
            }
        }
    });
}

/// A 32×32 purple circle with a white ring, drawn here so no image file is needed.
fn icon() -> Icon {
    const N: i32 = 32;
    let mut rgba = Vec::with_capacity((N * N * 4) as usize);
    for y in 0..N {
        for x in 0..N {
            let d = (((x - N / 2) * (x - N / 2) + (y - N / 2) * (y - N / 2)) as f32).sqrt();
            let px = if d < 9.0 { [255, 255, 255, 255] } else if d < 15.5 { [103, 65, 217, 255] } else { [0, 0, 0, 0] };
            rgba.extend_from_slice(&px);
        }
    }
    Icon::from_rgba(rgba, N as u32, N as u32).expect("icon")
}
