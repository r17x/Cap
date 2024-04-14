#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::{Arc};
use std::path::PathBuf;
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::Mutex;
use std::sync::atomic::{AtomicBool};
use std::{vec};
use tauri::{command, CustomMenuItem, Manager, Menu, MenuItem, Submenu, SystemTray, SystemTrayEvent, SystemTrayMenu, SystemTrayMenuItem, SystemTraySubmenu, Window};
use window_vibrancy::{apply_blur, apply_vibrancy, NSVisualEffectMaterial};
use window_shadows::set_shadow;
use tauri_plugin_positioner::{WindowExt, Position};
use tauri_plugin_oauth::start;

mod recording;
mod upload;
mod utils;
mod media;

use recording::{RecordingState, start_dual_recording, stop_all_recordings};
use upload::upload_file;
use media::{enumerate_audio_devices};
use utils::{has_screen_capture_access};

use ffmpeg_sidecar::{
    command::ffmpeg_is_installed,
    download::{check_latest_version, download_ffmpeg_package, ffmpeg_download_url, unpack_ffmpeg},
    error::Result as FfmpegResult,
    paths::sidecar_dir,
    version::ffmpeg_version,
};

use winit::monitor::{MonitorHandle, VideoMode};


fn main() {    
    let _ = fix_path_env::fix();
    
    std::panic::set_hook(Box::new(|info| {
        eprintln!("Thread panicked: {:?}", info);
    }));

    fn handle_ffmpeg_installation() -> FfmpegResult<()> {
        if ffmpeg_is_installed() {
            println!("FFmpeg is already installed! 🎉");
            return Ok(());
        }

        match check_latest_version() {
            Ok(version) => println!("Latest available version: {}", version),
            Err(_) => println!("Skipping version check on this platform."),
        }

        let download_url = ffmpeg_download_url()?;
        let destination = sidecar_dir()?;

        println!("Downloading from: {:?}", download_url);
        let archive_path = download_ffmpeg_package(download_url, &destination)?;
        println!("Downloaded package: {:?}", archive_path);

        println!("Extracting...");
        unpack_ffmpeg(&archive_path, &destination)?;

        let version = ffmpeg_version()?;
        println!("FFmpeg version: {}", version);

        println!("Done! 🏁");
        Ok(())
    }

    handle_ffmpeg_installation().expect("Failed to install FFmpeg");

    #[command]
    async fn start_server(window: Window) -> Result<u16, String> {
        start(move |url| {
            let _ = window.emit("redirect_uri", url);
        })
        .map_err(|err| err.to_string())
    }

    #[tauri::command]
    fn open_screen_capture_preferences() {
        #[cfg(target_os = "macos")]
        std::process::Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")
            .spawn()
            .expect("failed to open system preferences");
    }

    #[tauri::command]
    fn open_mic_preferences() {
        #[cfg(target_os = "macos")]
        std::process::Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone")
            .spawn()
            .expect("failed to open system preferences");
    }

    #[tauri::command]
    fn open_camera_preferences() {
        #[cfg(target_os = "macos")]
        std::process::Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Camera")
            .spawn()
            .expect("failed to open system preferences");
    }

    #[tauri::command]
    fn reset_screen_permissions() {
        #[cfg(target_os = "macos")]
        std::process::Command::new("tccutil")
            .arg("reset")
            .arg("ScreenCapture")
            .arg("so.cap.desktop")
            .spawn()
            .expect("failed to reset screen permissions");
    }

    #[tauri::command]
    fn reset_microphone_permissions() {
        #[cfg(target_os = "macos")]
        std::process::Command::new("tccutil")
            .arg("reset")
            .arg("Microphone")
            .arg("so.cap.desktop")
            .spawn()
            .expect("failed to reset microphone permissions");
    }

    #[tauri::command]
    fn reset_camera_permissions() {
        #[cfg(target_os = "macos")]
        std::process::Command::new("tccutil")
            .arg("reset")
            .arg("Camera")
            .arg("so.cap.desktop")
            .spawn()
            .expect("failed to reset camera permissions");
    }

    let _guard = sentry::init(("https://efd3156d9c0a8a49bee3ee675bec80d8@o4506859771527168.ingest.us.sentry.io/4506859844403200", sentry::ClientOptions {
      release: sentry::release_name!(),
      ..Default::default()
    }));

    let event_loop = winit::event_loop::EventLoop::new().expect("Failed to create event loop");
    let monitor: MonitorHandle = event_loop.primary_monitor().expect("No primary monitor found");
    let video_modes: Vec<VideoMode> = monitor.video_modes().collect();

    let max_mode = video_modes.iter().max_by_key(|mode| mode.size().width * mode.size().height);

    let (max_width, max_height) = if let Some(max_mode) = max_mode {
        println!("Maximum resolution: {:?}", max_mode.size());
        (max_mode.size().width, max_mode.size().height)
    } else {
        println!("Failed to determine maximum resolution.");
        (0, 0)
    };

    fn create_tray_menu(submenus: Option<Vec<SystemTraySubmenu>>) -> Option<SystemTrayMenu> {
        let mut tray_menu = SystemTrayMenu::new().add_item(CustomMenuItem::new("show-window", "Show Cap"));

        if let Some(items) = submenus {
            for submenu in items {
                tray_menu = tray_menu.add_submenu(submenu);
            }
        }

        Option::Some(tray_menu.add_native_item(tauri::SystemTrayMenuItem::Separator).add_item(CustomMenuItem::new("quit".to_string(), "Quit")))
    }

    let tray = SystemTray::new().with_menu(create_tray_menu(None).expect("Failed to create tray menu")).with_menu_on_left_click(false).with_title("Cap");

    tauri::Builder::default()
        .plugin(tauri_plugin_oauth::init())
        .plugin(tauri_plugin_positioner::init())
        .setup(move |app| {
            let handle = app.handle();

            if let Some(options_window) = app.get_window("main") { 
              let _ = options_window.move_window(Position::Center);
              #[cfg(target_os = "macos")]
              apply_vibrancy(&options_window, NSVisualEffectMaterial::MediumLight, None, Some(16.0)).expect("Unsupported platform! 'apply_vibrancy' is only supported on macOS");

              #[cfg(target_os = "windows")]
              apply_blur(&options_window, Some((255, 255, 255, 255))).expect("Unsupported platform! 'apply_blur' is only supported on Windows");
            
              set_shadow(&options_window, true).expect("Unsupported platform!");
            }

            let data_directory = handle.path_resolver().app_data_dir().unwrap_or_else(|| PathBuf::new());
            let recording_state = RecordingState {
                media_process: None,
                upload_handles: Mutex::new(vec![]),
                recording_options: None,
                shutdown_flag: Arc::new(AtomicBool::new(false)),
                video_uploading_finished: Arc::new(AtomicBool::new(false)),
                audio_uploading_finished: Arc::new(AtomicBool::new(false)),
                data_dir: Some(data_directory),
                max_screen_width: max_width as usize,
                max_screen_height: max_height as usize,
            };

            app.manage(Arc::new(Mutex::new(recording_state)));

            let tray_handle = app.tray_handle().clone();
            app.listen_global("toggle-recording", move|event| {
                let is_recording: bool = serde_json::from_str(event.payload().expect("Error while openning event payload")).expect("Error while deserializing recording state from event payload");

                if is_recording {
                    tray_handle.set_icon(tauri::Icon::Raw(include_bytes!("../icons/tray-stop-icon.png").to_vec())).unwrap();
                } else {
                    tray_handle.set_icon(tauri::Icon::Raw(include_bytes!("../icons/tray-default-icon.png").to_vec())).unwrap();
                }
            });
            
            let tray_handle = app.tray_handle().clone();
            app.listen_global("input-devices-set", move|event| {
                #[derive(Debug, Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct Device {
                    device_id: String,
                    group_id: String,
                    kind: String,
                    label: String
                }

                let devices: Vec<Device> = serde_json::from_str(event.payload().expect("Error wile openning event payload")).expect("Error while deserializing media devices from event payload");

                let mut tray_vid_items = SystemTrayMenu::new();
                let mut tray_mic_items = SystemTrayMenu::new();

                for device in devices {
                    if device.kind == "videoinput" {
                        tray_vid_items = tray_vid_items.add_item(CustomMenuItem::new(format!("in_vid_{}", device.device_id), device.label));
                    } else {
                        tray_mic_items = tray_mic_items.add_item(CustomMenuItem::new(format!("in_mic_{}", device.device_id), device.label));
                    }
                }

                let new_menu = create_tray_menu(Some(
                    vec![SystemTraySubmenu::new("Camera", tray_vid_items), SystemTraySubmenu::new("Microphone", tray_mic_items)])
                ).unwrap();

                tray_handle.set_menu(new_menu).expect("Error while updating the tray menu items");
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            start_dual_recording,
            stop_all_recordings,
            enumerate_audio_devices,
            upload_file,
            start_server,
            open_screen_capture_preferences,
            open_mic_preferences,
            open_camera_preferences,
            has_screen_capture_access,
            reset_screen_permissions,
            reset_microphone_permissions,
            reset_camera_permissions,
        ])
        .plugin(tauri_plugin_context_menu::init())
        .system_tray(tray)
        .on_system_tray_event(move |app, event| match event {
            SystemTrayEvent::MenuItemClick { id, .. } => match id.as_str() {
                "show-window" => {
                    let window = app.get_window("main").expect("Error while trying to get the main window.");
                    window.show().unwrap();
                    window.set_focus().unwrap();
                }
                "quit" => {
                    app.exit(0);
                }
                other => {
                    println!("Menu Item Clicked: {other}");
                }
            },
            SystemTrayEvent::LeftClick { position: _, size: _, .. } => {
                app.emit_all("tray-on-left-click", Some(())).unwrap();
            },
            _ => {}
        })
        .run(tauri::generate_context!())
        .expect("Error while running tauri application");
}
