use std::{
    collections::HashMap,
    iter::FromIterator,
    sync::{Arc, Mutex},
};

use sciter::Value;

use hbb_common::{
    allow_err,
    config::{LocalConfig, PeerConfig},
    log,
};

#[cfg(not(feature = "flutter"))]
use crate::ui_session_interface::Session;
use crate::{common::get_app_name, ipc, ui_interface::*};

mod cm;
#[cfg(feature = "inline")]
pub mod inline;
pub mod remote;

#[allow(dead_code)]
type Status = (i32, bool, i64, String);

lazy_static::lazy_static! {
    // stupid workaround for https://sciter.com/forums/topic/crash-on-latest-tis-mac-sdk-sometimes/
    static ref STUPID_VALUES: Mutex<Vec<Arc<Vec<Value>>>> = Default::default();
}

#[cfg(not(feature = "flutter"))]
lazy_static::lazy_static! {
    pub static ref CUR_SESSION: Arc<Mutex<Option<Session<remote::SciterHandler>>>> = Default::default();
}

struct UIHostHandler;

pub fn start(args: &mut [String]) {
    #[cfg(target_os = "macos")]
    crate::platform::delegate::show_dock();
    #[cfg(all(target_os = "linux", feature = "inline"))]
    {
        let app_dir = std::env::var("APPDIR").unwrap_or("".to_string());
        let mut so_path = "/usr/share/rustdesk/libsciter-gtk.so".to_owned();
        for (prefix, dir) in [
            ("", "/usr"),
            ("", "/app"),
            (&app_dir, "/usr"),
            (&app_dir, "/app"),
        ]
        .iter()
        {
            let path = format!("{prefix}{dir}/share/rustdesk/libsciter-gtk.so");
            if std::path::Path::new(&path).exists() {
                so_path = path;
                break;
            }
        }
        sciter::set_library(&so_path).ok();
    }
    #[cfg(windows)]
    // Check if there is a sciter.dll nearby.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let sciter_dll_path = parent.join("sciter.dll");
            if sciter_dll_path.exists() {
                // Try to set the sciter dll.
                let p = sciter_dll_path.to_string_lossy().to_string();
                log::debug!("Found dll:{}, \n {:?}", p, sciter::set_library(&p));
            }
        }
    }
    // https://github.com/c-smile/sciter-sdk/blob/master/include/sciter-x-types.h
    // https://github.com/rustdesk/rustdesk/issues/132#issuecomment-886069737
    #[cfg(windows)]
    allow_err!(sciter::set_options(sciter::RuntimeOptions::GfxLayer(
        sciter::GFX_LAYER::WARP
    )));
    use sciter::SCRIPT_RUNTIME_FEATURES::*;
    allow_err!(sciter::set_options(sciter::RuntimeOptions::ScriptFeatures(
        ALLOW_FILE_IO as u8 | ALLOW_SOCKET_IO as u8 | ALLOW_EVAL as u8 | ALLOW_SYSINFO as u8
    )));
    let mut frame = sciter::WindowBuilder::main_window().create();
    #[cfg(windows)]
    allow_err!(sciter::set_options(sciter::RuntimeOptions::UxTheming(true)));
    frame.set_title(&crate::get_app_name());
    #[cfg(target_os = "macos")]
    crate::platform::delegate::make_menubar(frame.get_host(), args.is_empty());
    #[cfg(windows)]
    crate::platform::try_set_window_foreground(frame.get_hwnd() as _);
    let page;
    if args.len() > 1 && args[0] == "--play" {
        args[0] = "--connect".to_owned();
        let path: std::path::PathBuf = (&args[1]).into();
        let id = path
            .file_stem()
            .map(|p| p.to_str().unwrap_or(""))
            .unwrap_or("")
            .to_owned();
        args[1] = id;
    }
    if args.is_empty() {
        std::thread::spawn(move || check_zombie());
        crate::common::check_software_update();
        frame.event_handler(UI {});
        frame.sciter_handler(UIHostHandler {});
        page = "index.html";
        // Start pulse audio local server.
        #[cfg(target_os = "linux")]
        std::thread::spawn(crate::ipc::start_pa);
    } else if args[0] == "--install" {
        frame.event_handler(UI {});
        frame.sciter_handler(UIHostHandler {});
        page = "install.html";
    } else if args[0] == "--cm" {
        frame.register_behavior("connection-manager", move || {
            Box::new(cm::SciterConnectionManager::new())
        });
        page = "cm.html";
        *cm::HIDE_CM.lock().unwrap() = crate::ipc::get_config("hide_cm")
            .ok()
            .flatten()
            .unwrap_or_default()
            == "true";
    } else if (args[0] == "--connect"
        || args[0] == "--file-transfer"
        || args[0] == "--port-forward"
        || args[0] == "--rdp")
        && args.len() > 1
    {
        #[cfg(windows)]
        {
            let hw = frame.get_host().get_hwnd();
            crate::platform::windows::enable_lowlevel_keyboard(hw as _);
        }
        let mut iter = args.iter();
        let Some(cmd) = iter.next() else {
            log::error!("Failed to get cmd arg");
            return;
        };
        let cmd = cmd.to_owned();
        let Some(id) = iter.next() else {
            log::error!("Failed to get id arg");
            return;
        };
        let id = id.to_owned();
        let pass = iter.next().unwrap_or(&"".to_owned()).clone();
        let args: Vec<String> = iter.map(|x| x.clone()).collect();
        frame.set_title(&id);
        frame.register_behavior("native-remote", move || {
            let handler =
                remote::SciterSession::new(cmd.clone(), id.clone(), pass.clone(), args.clone());
            #[cfg(not(feature = "flutter"))]
            {
                *CUR_SESSION.lock().unwrap() = Some(handler.inner());
            }
            Box::new(handler)
        });
        page = "remote.html";
    } else {
        log::error!("Wrong command: {:?}", args);
        return;
    }
    #[cfg(feature = "inline")]
    {
        let html = if page == "index.html" {
            inline::get_index()
        } else if page == "cm.html" {
            inline::get_cm()
        } else if page == "install.html" {
            inline::get_install()
        } else {
            inline::get_remote()
        };
        frame.load_html(html.as_bytes(), Some(page));
    }
    #[cfg(not(feature = "inline"))]
    frame.load_file(&format!(
        "file://{}/src/ui/{}",
        std::env::current_dir()
            .map(|c| c.display().to_string())
            .unwrap_or("".to_owned()),
        page
    ));
    let hide_cm = *cm::HIDE_CM.lock().unwrap();
    if !args.is_empty() && args[0] == "--cm" && hide_cm {
        // run_app calls expand(show) + run_loop, we use collapse(hide) + run_loop instead to create a hidden window
        frame.collapse(true);
        frame.run_loop();
        return;
    }
    frame.run_app();
}

struct UI {}

impl UI {
    fn recent_sessions_updated(&self) -> bool {
        recent_sessions_updated()
    }

    fn get_id(&self) -> String {
        ipc::get_id()
    }

    fn temporary_password(&mut self) -> String {
        temporary_password()
    }

    fn update_temporary_password(&self) {
        update_temporary_password()
    }

    fn set_permanent_password(&self, password: String) {
        let _ = set_permanent_password_with_result(password);
    }

    fn is_local_permanent_password_set(&self) -> bool {
        is_local_permanent_password_set()
    }

    fn is_permanent_password_set(&self) -> bool {
        is_permanent_password_set()
    }

    fn get_remote_id(&mut self) -> String {
        LocalConfig::get_remote_id()
    }

    fn set_remote_id(&mut self, id: String) {
        LocalConfig::set_remote_id(&id);
    }

    fn goto_install(&mut self) {
        goto_install();
    }

    fn install_me(&mut self, _options: String, _path: String) {
        install_me(_options, _path, false, false);
    }

    fn update_me(&self, _path: String) {
        update_me(_path);
    }

    fn run_without_install(&self) {
        run_without_install();
    }

    fn show_run_without_install(&self) -> bool {
        show_run_without_install()
    }

    fn get_license(&self) -> String {
        get_license()
    }

    fn get_option(&self, key: String) -> String {
        get_option(key)
    }

    fn get_local_option(&self, key: String) -> String {
        get_local_option(key)
    }

    fn set_local_option(&self, key: String, value: String) {
        set_local_option(key, value);
    }

    fn peer_has_password(&self, id: String) -> bool {
        peer_has_password(id)
    }

    fn forget_password(&self, id: String) {
        forget_password(id)
    }

    fn get_peer_option(&self, id: String, name: String) -> String {
        get_peer_option(id, name)
    }

    fn set_peer_option(&self, id: String, name: String, value: String) {
        set_peer_option(id, name, value)
    }

    fn using_public_server(&self) -> bool {
        crate::using_public_server()
    }

    fn is_incoming_only(&self) -> bool {
        hbb_common::config::is_incoming_only()
    }

    pub fn is_outgoing_only(&self) -> bool {
        hbb_common::config::is_outgoing_only()
    }

    pub fn is_custom_client(&self) -> bool {
        crate::common::is_custom_client()
    }

    pub fn is_disable_settings(&self) -> bool {
        hbb_common::config::is_disable_settings()
    }

    pub fn is_disable_account(&self) -> bool {
        hbb_common::config::is_disable_account()
    }

    pub fn is_disable_installation(&self) -> bool {
        hbb_common::config::is_disable_installation()
    }

    pub fn is_disable_ab(&self) -> bool {
        hbb_common::config::is_disable_ab()
    }

    fn get_options(&self) -> Value {
        let hashmap: HashMap<String, String> =
            serde_json::from_str(&get_options()).unwrap_or_default();
        let mut m = Value::map();
        for (k, v) in hashmap {
            m.set_item(k, v);
        }
        m
    }

    fn test_if_valid_server(&self, host: String, test_with_proxy: bool) -> String {
        test_if_valid_server(host, test_with_proxy)
    }

    fn get_sound_inputs(&self) -> Value {
        Value::from_iter(get_sound_inputs())
    }

    fn set_options(&self, v: Value) {
        let mut m = HashMap::new();
        for (k, v) in v.items() {
            if let Some(k) = k.as_string() {
                if let Some(v) = v.as_string() {
                    if !v.is_empty() {
                        m.insert(k, v);
                    }
                }
            }
        }
        set_options(m);
    }

    fn set_option(&self, key: String, value: String) {
        set_option(key, value);
    }

    fn install_path(&mut self) -> String {
        install_path()
    }

    fn install_options(&self) -> String {
        install_options()
    }

    fn get_socks(&self) -> Value {
        Value::from_iter(get_socks())
    }

    fn set_socks(&self, proxy: String, username: String, password: String) {
        set_socks(proxy, username, password)
    }

    fn is_installed(&self) -> bool {
        is_installed()
    }

    fn get_supported_privacy_mode_impls(&self) -> String {
        serde_json::to_string(&crate::privacy_mode::get_supported_privacy_mode_impl())
            .unwrap_or_default()
    }

    fn is_root(&self) -> bool {
        is_root()
    }

    fn is_release(&self) -> bool {
        #[cfg(not(debug_assertions))]
        return true;
        #[cfg(debug_assertions)]
        return false;
    }

    fn is_share_rdp(&self) -> bool {
        is_share_rdp()
    }

    fn set_share_rdp(&self, _enable: bool) {
        set_share_rdp(_enable);
    }

    fn is_installed_lower_version(&self) -> bool {
        is_installed_lower_version()
    }

    fn closing(&mut self, x: i32, y: i32, w: i32, h: i32) {
        crate::server::input_service::fix_key_down_timeout_at_exit();
        LocalConfig::set_size(x, y, w, h);
    }

    fn get_size(&mut self) -> Value {
        let s = LocalConfig::get_size();
        let mut v = Vec::new();
        v.push(s.0);
        v.push(s.1);
        v.push(s.2);
        v.push(s.3);
        Value::from_iter(v)
    }

    fn get_mouse_time(&self) -> f64 {
        get_mouse_time()
    }

    fn check_mouse_time(&self) {
        check_mouse_time()
    }

    fn get_connect_status(&mut self) -> Value {
        let mut v = Value::array(0);
        let x = get_connect_status();
        v.push(x.status_num);
        v.push(x.key_confirmed);
        v.push(x.id);
        v
    }

    #[inline]
    fn get_peer_value(id: String, p: PeerConfig) -> Value {
        let values = vec![
            id,
            p.info.username.clone(),
            p.info.hostname.clone(),
            p.info.platform.clone(),
            p.options.get("alias").unwrap_or(&"".to_owned()).to_owned(),
        ];
        Value::from_iter(values)
    }

    fn get_peer(&self, id: String) -> Value {
        let c = get_peer(id.clone());
        Self::get_peer_value(id, c)
    }

    fn get_fav(&self) -> Value {
        Value::from_iter(get_fav())
    }

    fn store_fav(&self, fav: Value) {
        let mut tmp = vec![];
        fav.values().for_each(|v| {
            if let Some(v) = v.as_string() {
                if !v.is_empty() {
                    tmp.push(v);
                }
            }
        });
        store_fav(tmp);
    }

    fn get_recent_sessions(&mut self) -> Value {
        // to-do: limit number of recent sessions, and remove old peer file
        let peers: Vec<Value> = PeerConfig::peers(None)
            .drain(..)
            .map(|p| Self::get_peer_value(p.0, p.2))
            .collect();
        Value::from_iter(peers)
    }

    fn get_icon(&mut self) -> String {
        get_icon()
    }

    fn remove_peer(&mut self, id: String) {
        PeerConfig::remove(&id);
    }

    fn remove_discovered(&mut self, id: String) {
        remove_discovered(id);
    }

    fn send_wol(&mut self, id: String) {
        crate::lan::send_wol(id)
    }

    fn new_remote(&mut self, id: String, remote_type: String, force_relay: bool) {
        new_remote(id, remote_type, force_relay)
    }

    fn is_process_trusted(&mut self, _prompt: bool) -> bool {
        is_process_trusted(_prompt)
    }

    fn is_can_screen_recording(&mut self, _prompt: bool) -> bool {
        is_can_screen_recording(_prompt)
    }

    fn is_installed_daemon(&mut self, _prompt: bool) -> bool {
        is_installed_daemon(_prompt)
    }

    fn get_error(&mut self) -> String {
        get_error()
    }

    fn is_login_wayland(&mut self) -> bool {
        is_login_wayland()
    }

    fn current_is_wayland(&mut self) -> bool {
        current_is_wayland()
    }

    fn get_software_update_url(&self) -> String {
        crate::SOFTWARE_UPDATE_URL.lock().unwrap().clone()
    }

    fn get_new_version(&self) -> String {
        get_new_version()
    }

    fn get_version(&self) -> String {
        get_version()
    }

    fn get_fingerprint(&self) -> String {
        get_fingerprint()
    }

    fn get_app_name(&self) -> String {
        get_app_name()
    }

    fn get_software_ext(&self) -> String {
        #[cfg(windows)]
        let p = "exe";
        #[cfg(target_os = "macos")]
        let p = "dmg";
        #[cfg(target_os = "linux")]
        let p = "deb";
        p.to_owned()
    }

    fn get_software_store_path(&self) -> String {
        let mut p = std::env::temp_dir();
        let name = crate::SOFTWARE_UPDATE_URL
            .lock()
            .unwrap()
            .split("/")
            .last()
            .map(|x| x.to_owned())
            .unwrap_or(crate::get_app_name());
        p.push(name);
        format!("{}.{}", p.to_string_lossy(), self.get_software_ext())
    }

    fn create_shortcut(&self, _id: String) {
        #[cfg(windows)]
        create_shortcut(_id)
    }

    fn discover(&self) {
        std::thread::spawn(move || {
            allow_err!(crate::lan::discover());
        });
    }

    fn get_lan_peers(&self) -> String {
        // let peers = get_lan_peers()
        //     .into_iter()
        //     .map(|mut peer| {
        //         (
        //             peer.remove("id").unwrap_or_default(),
        //             peer.remove("username").unwrap_or_default(),
        //             peer.remove("hostname").unwrap_or_default(),
        //             peer.remove("platform").unwrap_or_default(),
        //         )
        //     })
        //     .collect::<Vec<(String, String, String, String)>>();
        serde_json::to_string(&get_lan_peers()).unwrap_or_default()
    }

    fn get_uuid(&self) -> String {
        get_uuid()
    }

    fn open_url(&self, url: String) {
        #[cfg(windows)]
        let p = "explorer";
        #[cfg(target_os = "macos")]
        let p = "open";
        #[cfg(target_os = "linux")]
        let p = if std::path::Path::new("/usr/bin/firefox").exists() {
            "firefox"
        } else {
            "xdg-open"
        };
        allow_err!(std::process::Command::new(p).arg(url).spawn());
    }

    fn change_id(&self, id: String) {
        reset_async_job_status();
        let old_id = self.get_id();
        change_id_shared(id, old_id);
    }

    fn http_request(&self, url: String, method: String, body: Option<String>, header: String) {
        http_request(url, method, body, header)
    }

    fn post_request(&self, url: String, body: String, header: String) {
        post_request(url, body, header)
    }

    fn is_ok_change_id(&self) -> bool {
        hbb_common::machine_uid::get().is_ok()
    }

    fn get_async_job_status(&self) -> String {
        get_async_job_status()
    }

    fn get_http_status(&self, url: String) -> Option<String> {
        get_async_http_status(url)
    }

    fn t(&self, name: String) -> String {
        crate::client::translate(name)
    }

    fn is_xfce(&self) -> bool {
        crate::platform::is_xfce()
    }

    fn get_api_server(&self) -> String {
        get_api_server()
    }

    fn has_hwcodec(&self) -> bool {
        has_hwcodec()
    }

    fn has_vram(&self) -> bool {
        has_vram()
    }

    fn get_langs(&self) -> String {
        get_langs()
    }

    fn video_save_directory(&self, root: bool) -> String {
        video_save_directory(root)
    }

    fn handle_relay_id(&self, id: String) -> String {
        handle_relay_id(&id).to_owned()
    }

    fn get_login_device_info(&self) -> String {
        get_login_device_info_json()
    }

    fn support_remove_wallpaper(&self) -> bool {
        support_remove_wallpaper()
    }

    fn has_valid_2fa(&self) -> bool {
        has_valid_2fa()
    }

    fn generate2fa(&self) -> String {
        generate2fa()
    }

    pub fn verify2fa(&self, code: String) -> bool {
        verify2fa(code)
    }

    fn verify_login(&self, raw: String, id: String) -> bool {
        crate::verify_login(&raw, &id)
    }

    fn generate_2fa_img_src(&self, data: String) -> String {
        let v = qrcode_generator::to_png_to_vec(data, qrcode_generator::QrCodeEcc::Low, 128)
            .unwrap_or_default();
        let s = hbb_common::sodiumoxide::base64::encode(
            v,
            hbb_common::sodiumoxide::base64::Variant::Original,
        );
        format!("data:image/png;base64,{s}")
    }

    pub fn check_hwcodec(&self) {
        check_hwcodec()
    }

    fn is_option_fixed(&self, key: String) -> bool {
        crate::ui_interface::is_option_fixed(&key)
    }

    fn get_builtin_option(&self, key: String) -> String {
        crate::ui_interface::get_builtin_option(&key)
    }

    fn is_remote_modify_enabled_by_control_permissions(&self) -> String {
        match crate::ui_interface::is_remote_modify_enabled_by_control_permissions() {
            Some(true) => "true",
            Some(false) => "false",
            None => "",
        }
        .to_string()
    }
}

impl sciter::EventHandler for UI {
    sciter::dispatch_script_call! {
        fn t(String);
        fn get_api_server();
        fn is_xfce();
        fn using_public_server();
        fn is_custom_client();
        fn is_outgoing_only();
        fn is_incoming_only();
        fn is_disable_settings();
        fn is_disable_account();
        fn is_disable_installation();
        fn is_disable_ab();
        fn get_id();
        fn temporary_password();
        fn update_temporary_password();
        fn set_permanent_password(String);
        fn is_local_permanent_password_set();
        fn is_permanent_password_set();
        fn get_remote_id();
        fn set_remote_id(String);
        fn closing(i32, i32, i32, i32);
        fn get_size();
        fn new_remote(String, String, bool);
        fn send_wol(String);
        fn remove_peer(String);
        fn remove_discovered(String);
        fn get_connect_status();
        fn get_mouse_time();
        fn check_mouse_time();
        fn get_recent_sessions();
        fn get_peer(String);
        fn get_fav();
        fn store_fav(Value);
        fn recent_sessions_updated();
        fn get_icon();
        fn install_me(String, String);
        fn is_installed();
        fn get_supported_privacy_mode_impls();
        fn is_root();
        fn is_release();
        fn set_socks(String, String, String);
        fn get_socks();
        fn is_share_rdp();
        fn set_share_rdp(bool);
        fn is_installed_lower_version();
        fn install_path();
        fn install_options();
        fn goto_install();
        fn is_process_trusted(bool);
        fn is_can_screen_recording(bool);
        fn is_installed_daemon(bool);
        fn get_error();
        fn is_login_wayland();
        fn current_is_wayland();
        fn get_options();
        fn get_option(String);
        fn get_local_option(String);
        fn set_local_option(String, String);
        fn get_peer_option(String, String);
        fn peer_has_password(String);
        fn forget_password(String);
        fn set_peer_option(String, String, String);
        fn get_license();
        fn test_if_valid_server(String, bool);
        fn get_sound_inputs();
        fn set_options(Value);
        fn set_option(String, String);
        fn get_software_update_url();
        fn get_new_version();
        fn get_version();
        fn get_fingerprint();
        fn update_me(String);
        fn show_run_without_install();
        fn run_without_install();
        fn get_app_name();
        fn get_software_store_path();
        fn get_software_ext();
        fn open_url(String);
        fn change_id(String);
        fn get_async_job_status();
        fn post_request(String, String, String);
        fn is_ok_change_id();
        fn create_shortcut(String);
        fn discover();
        fn get_lan_peers();
        fn get_uuid();
        fn has_hwcodec();
        fn has_vram();
        fn get_langs();
        fn video_save_directory(bool);
        fn handle_relay_id(String);
        fn get_login_device_info();
        fn support_remove_wallpaper();
        fn has_valid_2fa();
        fn generate2fa();
        fn generate_2fa_img_src(String);
        fn verify2fa(String);
        fn check_hwcodec();
        fn verify_login(String, String);
        fn is_option_fixed(String);
        fn get_builtin_option(String);
        fn is_remote_modify_enabled_by_control_permissions();
    }
}

impl sciter::host::HostHandler for UIHostHandler {
    fn on_graphics_critical_failure(&mut self) {
        log::error!("Critical rendering error: e.g. DirectX gfx driver error. Most probably bad gfx drivers.");
    }
}

#[cfg(not(target_os = "linux"))]
fn get_sound_inputs() -> Vec<String> {
    let mut out = Vec::new();
    use cpal::traits::{DeviceTrait, HostTrait};
    let host = cpal::default_host();
    if let Ok(devices) = host.devices() {
        for device in devices {
            if device.default_input_config().is_err() {
                continue;
            }
            if let Ok(name) = device.name() {
                out.push(name);
            }
        }
    }
    out
}

#[cfg(target_os = "linux")]
fn get_sound_inputs() -> Vec<String> {
    crate::platform::linux::get_pa_sources()
        .drain(..)
        .map(|x| x.1)
        .collect()
}

// sacrifice some memory
pub fn value_crash_workaround(values: &[Value]) -> Arc<Vec<Value>> {
    let persist = Arc::new(values.to_vec());
    STUPID_VALUES.lock().unwrap().push(persist.clone());
    persist
}

pub fn get_icon() -> String {
    "data:image/png;base64,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".into()
}
