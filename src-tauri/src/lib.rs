//! Punto de entrada de la app Tauri. Configura el system tray, oculta la
//! ventana por defecto (el agente vive como icono de bandeja) y arranca el
//! servidor HTTP+WS en una tarea aparte.

use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager, WindowEvent,
};

mod amautum;
mod config;
mod diarize;
mod ffmpeg;
mod models;
mod pending;
mod pipeline;
mod server;
mod types;
mod updates;
mod whisper;
mod ws_hub;

/// Menú de la app en macOS.
///
/// Sin menú propio, macOS pone el suyo y su panel «Acerca de» solo muestra
/// icono, nombre y versión: no dice qué hace la app ni cómo pedir ayuda. Para
/// quien la abre por primera vez, ese panel es el sitio natural donde mirar.
///
/// Va en el BUILDER, no en `setup`: `AppHandle::set_menu` desde setup no llega a
/// sustituir el menú por defecto en macOS —se comprobó, y seguía saliendo
/// File/Edit/View/Window/Help—, mientras que `Builder::menu` sí.
#[cfg(target_os = "macos")]
fn build_app_menu(app: &tauri::AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    use tauri::menu::{AboutMetadataBuilder, Submenu};

    let about_metadata = AboutMetadataBuilder::new()
        .name(Some("Amautum Transcriptor"))
        .version(Some(config::version()))
        // Sin esto el panel «Acerca de» sale con el icono genérico de carpeta:
        // en `tauri dev` la app corre sin empaquetar y macOS no tiene de dónde
        // sacarlo. Pasándolo explícitamente sale bien también sin empaquetar, y
        // no hacen falta features extra de imagen — reusamos el que ya declara
        // `tauri.conf.json` como icono de ventana.
        .icon(app.default_window_icon().cloned())
        .comments(Some(
            "Transcribe audiencias y declaraciones en este mismo equipo.\n\n\
             El audio nunca sale de aquí: solo el texto se sube a Amautum cuando termina.",
        ))
        .copyright(Some("© Sugestion S.A.S"))
        .website(Some("https://www.amautum.com/transcriptor"))
        .website_label(Some("Ir a Amautum Transcriptor"))
        .build();

    let app_menu = Submenu::with_items(
        app,
        "Amautum Transcriptor",
        true,
        &[
            &PredefinedMenuItem::about(
                app,
                Some("Acerca de Amautum Transcriptor"),
                Some(about_metadata),
            )?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, "support", "Ayuda y soporte…", true, None::<&str>)?,
            &MenuItem::with_id(app, "downloads", "Guía de instalación…", true, None::<&str>)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::hide(app, None)?,
            &PredefinedMenuItem::hide_others(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::quit(app, None)?,
        ],
    )?;

    // Edición no es decorativo: sin él, Cmd+C no copia el texto de la bitácora,
    // que es justo lo que se pega en un ticket de soporte.
    let edit_menu = Submenu::with_items(
        app,
        "Edición",
        true,
        &[
            &PredefinedMenuItem::copy(app, None)?,
            &PredefinedMenuItem::paste(app, None)?,
            &PredefinedMenuItem::select_all(app, None)?,
        ],
    )?;

    let help_menu = Submenu::with_items(
        app,
        "Ayuda",
        true,
        &[
            &MenuItem::with_id(app, "open", "Ventana de estado", true, None::<&str>)?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, "support", "Escribir a soporte…", true, None::<&str>)?,
            &MenuItem::with_id(app, "notes", "Novedades de esta versión…", true, None::<&str>)?,
        ],
    )?;

    Menu::with_items(app, &[&app_menu, &edit_menu, &help_menu])
}

/// Qué hace cada entrada de menú.
///
/// Una sola función para la bandeja y para el menú de la app: las dos ofrecen
/// «Ayuda y soporte» y tienen que llevar exactamente al mismo sitio. Duplicar
/// el `match` era garantizar que un día divergieran.
fn menu_action(app: &tauri::AppHandle, id: &str) {
    use tauri_plugin_opener::OpenerExt;

    match id {
        "open" => {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.set_focus();
            }
        }
        "support" => {
            // Con el contexto puesto: versión, sistema y el último fallo si lo
            // hubo. Quien pide ayuda no debería tener que transcribir a mano un
            // mensaje de error.
            let ctx = app
                .try_state::<pipeline::AppState>()
                .and_then(|s| s.last_error());
            let _ = app
                .opener()
                .open_url(config::support_url(ctx.as_deref()), None::<String>);
        }
        "downloads" => {
            let _ = app.opener().open_url(config::downloads_url(), None::<String>);
        }
        "notes" => {
            let _ = app.opener().open_url(updates::release_notes_url(), None::<String>);
        }
        "amautum" => {
            let _ = app.opener().open_url("https://www.amautum.com", None::<String>);
        }
        "quit" => app.exit(0),
        _ => {}
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,amautum_agent_lib=debug".into()),
        )
        .try_init();

    #[allow(unused_mut)]
    let mut builder = tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .on_menu_event(|app, event| menu_action(app, event.id().as_ref()));

    // Solo en macOS: en Windows y Linux el menú se dibuja DENTRO de la ventana, y
    // esta es un panel de estado pequeño donde una barra de menús sobra. Ahí la
    // misma información vive en la propia ventana.
    #[cfg(target_os = "macos")]
    {
        builder = builder.menu(build_app_menu);
    }

    builder
        .setup(|app| {
            let state = pipeline::AppState::new();
            app.manage(state.clone());

            // Menú de la bandeja.
            //
            // El icono de la bandeja es, para mucha gente, la ÚNICA superficie de
            // la app: se instala, se deja corriendo y no se vuelve a abrir la
            // ventana. Si soporte no está aquí, no está en ninguna parte para
            // quien no sabe que existe una ventana de estado.
            //
            // Orden pensado por frecuencia de uso, con lo destructivo (Salir)
            // separado al final para no pulsarlo por error.
            let open_item =
                MenuItem::with_id(app, "open", "Abrir ventana de estado", true, None::<&str>)?;
            let support_item =
                MenuItem::with_id(app, "support", "Ayuda y soporte…", true, None::<&str>)?;
            let amautum_item =
                MenuItem::with_id(app, "amautum", "Abrir amautum.com", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "Salir", true, None::<&str>)?;
            let menu = Menu::with_items(
                app,
                &[
                    &open_item,
                    &PredefinedMenuItem::separator(app)?,
                    &support_item,
                    &amautum_item,
                    &PredefinedMenuItem::separator(app)?,
                    &quit_item,
                ],
            )?;

            let _tray = TrayIconBuilder::new()
                .menu(&menu)
                .show_menu_on_left_click(false)
                .tooltip("Amautum Transcriptor")
                .on_menu_event(|app, event| menu_action(app, event.id().as_ref()))
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

            // Health check de los sidecars al arrancar. `dependencies_ok`
            // arranca en `true` (optimista) y SOLO bajamos a `false` si el
            // probe confirma un problema. Si por cualquier motivo el probe
            // mismo no se puede correr (sidecar perdido al spawn), eso cuenta
            // como confirmación. Si tarda 200 ms en correr, el navegador no
            // ve un `false` espurio durante esos 200 ms.
            let app_for_probe = app.handle().clone();
            let state_for_probe = state.clone();
            tauri::async_runtime::spawn(async move {
                let ffmpeg = models::probe_sidecar(&app_for_probe, config::FFMPEG_SIDECAR).await;
                let whisper = models::probe_sidecar(&app_for_probe, config::WHISPER_SIDECAR).await;
                // El diarizador es OPCIONAL: su ausencia NO baja `dependencies_ok`
                // (un estudio que solo transcribe sin identificar oradores debe
                // poder usar el agente igual). Solo lo logueamos como información.
                let diarize = models::probe_sidecar(&app_for_probe, config::DIARIZE_SIDECAR).await;
                tracing::info!(sherpa_diarize = ?diarize, "Probe del sidecar de diarización (opcional)");

                // Guardamos el DETALLE del fallo, no solo un booleano: "faltan
                // componentes" no le sirve a nadie. Con el detalle, la ventana
                // puede decir «al motor le falta el Visual C++ Redistributable»
                // y dar el enlace de descarga.
                let mut problems: Vec<String> = Vec::new();
                if let Err(err) = &ffmpeg {
                    problems.push(format!("Conversor de audio (ffmpeg): {err}"));
                }
                if let Err(err) = &whisper {
                    problems.push(format!("Motor de transcripción (whisper): {err}"));
                }

                if problems.is_empty() {
                    state_for_probe.log(
                        "info",
                        format!(
                            "Componentes verificados — ffmpeg: {} · motor: {}",
                            ffmpeg.as_deref().unwrap_or("?"),
                            whisper.as_deref().unwrap_or("?")
                        ),
                    );
                    *state_for_probe.dependencies_error.lock() = None;
                    // Reafirmamos `true` por si alguien lo cambió. No-op si ya estaba.
                    state_for_probe
                        .dependencies_ok
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                } else {
                    let detail = problems.join(" · ");
                    state_for_probe.log("error", format!("Componentes con problemas: {detail}"));
                    *state_for_probe.dependencies_error.lock() = Some(detail);
                    state_for_probe
                        .dependencies_ok
                        .store(false, std::sync::atomic::Ordering::SeqCst);
                }
            });

            // Aviso de versión nueva. Amautum ya avisa en el navegador, pero el
            // agente vive en la bandeja durante semanas y quien no abre el
            // asistente nunca se entera. Preguntamos al arrancar (con un respiro
            // para no competir con el arranque) y cada 6 h.
            let state_for_updates = state.clone();
            let app_for_updates = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(20)).await;
                loop {
                    if let Some(info) = updates::check().await {
                        let available = info.available;
                        if available {
                            state_for_updates.log(
                                "info",
                                format!(
                                    "Hay una versión nueva del agente: v{} (tienes la v{}).",
                                    info.latest, info.current
                                ),
                            );
                        }
                        *state_for_updates.update.lock() = Some(info);

                        // Actualización silenciosa: solo con el agente OCIOSO.
                        // `install_update` reinicia y no vuelve si lo consigue;
                        // si hay trabajo en curso devuelve error y se reintenta
                        // en el siguiente ciclo. Ver el comentario largo de
                        // updates.rs — reiniciar a mitad de una transcripción
                        // destruiría horas de CPU.
                        if available {
                            let _ = updates::install_update(
                                &app_for_updates,
                                &state_for_updates,
                                true,
                            )
                            .await;
                        }
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(6 * 60 * 60)).await;
                }
            });

            // Reintento de actas pendientes de subir: si una transcripción
            // terminó pero no se pudo subir (sin internet), quedó guardada en
            // disco. La reintentamos al arrancar (tras un respiro para que la
            // red levante) y luego cada 5 min. Así un corte de conexión no
            // pierde el trabajo: el job se completa solo cuando vuelve la red.
            let app_for_pending = app.handle().clone();
            let ws_for_pending = state.ws_hub.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                loop {
                    pending::retry_all(&app_for_pending, &ws_for_pending).await;
                    tokio::time::sleep(std::time::Duration::from_secs(5 * 60)).await;
                }
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("Error al arrancar la app Tauri del agente");
}
