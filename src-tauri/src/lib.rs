//! Punto de entrada de la app Tauri. Configura el system tray, oculta la
//! ventana por defecto (el agente vive como icono de bandeja) y arranca el
//! servidor HTTP+WS en una tarea aparte.

use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager, WindowEvent,
};

mod amautum;
mod config;
mod ffmpeg;
mod pipeline;
mod server;
mod types;
mod whisper;
mod ws_hub;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,amautum_agent_lib=debug".into()),
        )
        .try_init();

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let state = pipeline::AppState::new();
            app.manage(state.clone());

            // Tray con un menú mínimo: estado / abrir Amautum / abrir ventana /
            // salir.
            let open_item =
                MenuItem::with_id(app, "open", "Abrir ventana de estado", true, None::<&str>)?;
            let amautum_item =
                MenuItem::with_id(app, "amautum", "Abrir amautum.com", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "Salir", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open_item, &amautum_item, &quit_item])?;

            let _tray = TrayIconBuilder::new()
                .menu(&menu)
                .show_menu_on_left_click(false)
                .tooltip("Amautum Transcriptor")
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "open" => {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                    "amautum" => {
                        use tauri_plugin_opener::OpenerExt;
                        let _ = app.opener().open_url("https://www.amautum.com", None::<String>);
                    }
                    "quit" => {
                        app.exit(0);
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                })
                .build(app)?;

            // Cerrar la ventana de estado la oculta en vez de matar al agente.
            // El usuario sale solo desde el menú "Salir" del tray.
            if let Some(window) = app.get_webview_window("main") {
                let w = window.clone();
                window.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        let _ = w.hide();
                        api.prevent_close();
                    }
                });
            }

            // Arranca el servidor HTTP+WS en una tarea Tokio. Si falla,
            // notificamos por log y dejamos el agente vivo para que el usuario
            // vea el estado de "Servidor local no responde" en la ventana.
            let app_handle = app.handle().clone();
            let state_for_server = state.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(err) = server::run(app_handle, state_for_server).await {
                    tracing::error!(?err, "El servidor HTTP del agente terminó con error");
                }
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("Error al arrancar la app Tauri del agente");
}
