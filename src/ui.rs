use crate::{App, Event, preferences::Preferences};
use anyhow::{Context, Result, ensure};
use std::{
    io::{Seek, SeekFrom, Write},
    os::fd::AsFd,
    sync::mpsc,
    time::Duration,
};
use wayland_client::{
    Connection, Dispatch, QueueHandle, delegate_noop,
    protocol::{
        wl_buffer, wl_compositor, wl_keyboard, wl_output, wl_pointer, wl_registry, wl_seat, wl_shm, wl_shm_pool,
        wl_surface,
    },
};
use xkbcommon::xkb;
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1, zwp_virtual_keyboard_v1,
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{self, Layer},
    zwlr_layer_surface_v1::{self, Anchor, KeyboardInteractivity},
};

#[derive(Clone, Debug)]
pub struct OutputItem {
    pub name: String,
    pub output: wl_output::WlOutput,
}

pub struct Ui {
    connection: Connection,
    qh: QueueHandle<Self>,
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    seat: Option<wl_seat::WlSeat>,
    shell: Option<zwlr_layer_shell_v1::ZwlrLayerShellV1>,
    manager: Option<zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1>,
    keyboard: Option<zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1>,
    surface: Option<wl_surface::WlSurface>,
    layer: Option<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1>,
    pointer: Option<wl_pointer::WlPointer>,
    tx: mpsc::Sender<Event>,
    pub capturing_keyboard: bool,
    pub arm_token: u64,
    pub seen_enter: bool,
    pub seen_modifiers: bool,
    pub pressed_keys: std::collections::BTreeSet<u32>,
    pub mods_depressed: u32,
    pub arming: bool,
    pub arm_target: Option<(u64, String, bool, std::time::Instant)>,
    seat_keyboard: Option<wl_keyboard::WlKeyboard>,
    xkb_context: xkb::Context,
    xkb_state: Option<xkb::State>,
    configured: bool,
    closed: bool,
    color: [u8; 3],
    pub models: Vec<String>,
    pub message: String,
    draft: crate::settings::Settings,
    saved: crate::settings::Settings,
    preferences: Option<Preferences>,
    dock_reconfigure: bool,
    pointer_xy: (f64, f64),
    press: Option<(f64, f64, i32)>,
    dragged: bool,
    tick: f64,
    open_progress: f64,
    pub level: f64,
    pub microphones: Vec<crate::settings::Microphone>,
    pub monitors: Vec<crate::settings::MonitorInfo>,
    outputs: Vec<OutputItem>,
    buffers: usize,
    pub dirty: bool,
    max_x: i32,
}
impl Ui {
    fn new(
        connection: Connection,
        qh: QueueHandle<Self>,
        app: &App,
        microphones: Vec<crate::settings::Microphone>,
        monitors: Vec<crate::settings::MonitorInfo>,
        max_x: i32,
    ) -> Self {
        Self {
            connection,
            qh,
            compositor: None,
            shm: None,
            seat: None,
            shell: None,
            manager: None,
            keyboard: None,
            surface: None,
            layer: None,
            pointer: None,
            tx: app.tx.clone(),
            capturing_keyboard: false,
            arm_token: 0,
            seen_enter: false,
            seen_modifiers: false,
            pressed_keys: std::collections::BTreeSet::new(),
            mods_depressed: 0,
            arming: false,
            arm_target: None,
            seat_keyboard: None,
            xkb_context: xkb::Context::new(xkb::CONTEXT_NO_FLAGS),
            xkb_state: None,
            configured: false,
            closed: false,
            color: app.color(),
            models: Vec::new(),
            message: "Cargando catálogo…".into(),
            draft: app.settings.clone(),
            saved: app.settings.clone(),
            preferences: None,
            dock_reconfigure: false,
            pointer_xy: (0., 0.),
            press: None,
            dragged: false,
            tick: 0.,
            open_progress: 0.,
            level: 0.,
            microphones,
            monitors,
            outputs: Vec::new(),
            buffers: 0,
            dirty: true,
            max_x,
        }
    }
    #[cfg(test)]
    pub fn new_for_test(connection: Connection, qh: QueueHandle<Self>, tx: mpsc::Sender<Event>) -> Self {
        Self {
            connection,
            qh,
            compositor: None,
            shm: None,
            seat: None,
            shell: None,
            manager: None,
            keyboard: None,
            surface: None,
            layer: None,
            pointer: None,
            tx,
            capturing_keyboard: false,
            arm_token: 0,
            seen_enter: false,
            seen_modifiers: false,
            pressed_keys: std::collections::BTreeSet::new(),
            mods_depressed: 0,
            arming: false,
            arm_target: None,
            seat_keyboard: None,
            xkb_context: xkb::Context::new(xkb::CONTEXT_NO_FLAGS),
            xkb_state: None,
            configured: false,
            closed: false,
            color: [55, 170, 130],
            models: Vec::new(),
            message: String::new(),
            draft: crate::settings::Settings::default(),
            saved: crate::settings::Settings::default(),
            preferences: None,
            dock_reconfigure: false,
            pointer_xy: (0., 0.),
            press: None,
            dragged: false,
            tick: 0.,
            open_progress: 0.,
            level: 0.,
            microphones: Vec::new(),
            monitors: Vec::new(),
            outputs: Vec::new(),
            buffers: 0,
            dirty: true,
            max_x: 1280,
        }
    }
    pub fn settings_saved(&mut self, settings: &crate::settings::Settings) {
        self.dock_reconfigure = self.saved.monitor != settings.monitor;
        self.saved = settings.clone();
        self.draft = settings.clone();
    }
    pub fn server_saved(&mut self, base_url: &str) {
        self.saved.base_url = base_url.into();
        self.draft.base_url = base_url.into();
        self.models.clear();
        if let Some(preferences) = &self.preferences {
            preferences.server_saved(base_url);
        }
        self.message = "Servidor guardado · cargando catálogo…".into();
        self.dirty = true;
    }
    pub fn position_saved(&mut self, x: i32) {
        self.saved.x = x;
        self.draft.x = x;
        if let Some(preferences) = &self.preferences {
            preferences.position_saved(x);
        }
    }
    fn save_position(&self) {
        let _ = self.tx.send(Event::SavePosition(self.draft.x));
    }
    fn animate_dock(&mut self, color: [u8; 3]) {
        self.color = color;
        self.tick += 1.;
        self.dirty = true;
    }
    fn attach(
        &mut self,
        surface: &wl_surface::WlSurface,
        width: i32,
        height: i32,
        pixels: &[u8],
        qh: &QueueHandle<Self>,
    ) -> Result<()> {
        let mut file = tempfile::tempfile()?;
        file.write_all(pixels)?;
        let pool =
            self.shm
                .as_ref()
                .unwrap()
                .create_pool(file.as_fd(), pixels.len() as i32, qh, ());
        let buffer = pool.create_buffer(
            0,
            width,
            height,
            width * 4,
            wl_shm::Format::Argb8888,
            qh,
            (),
        );
        self.buffers += 1;
        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, width, height);
        surface.commit();
        pool.destroy();
        Ok(())
    }
    fn draw(&mut self, qh: &QueueHandle<Self>) -> Result<()> {
        if self.buffers >= 4 {
            return Ok(());
        }
        self.dirty = false;
        if self.configured {
            let pixels = crate::paint::shell(
                176,
                44,
                self.color,
                self.open_progress,
                self.level,
                self.tick,
            )?;
            let surface = self.surface.as_ref().unwrap().clone();
            self.attach(&surface, 176, 44, &pixels, qh)?;
        }
        if let Some(preferences) = &self.preferences {
            preferences.set_message(&self.message);
        }
        Ok(())
    }
    pub fn close_preferences(&mut self) {
        if let Some(preferences) = self.preferences.take()
            && preferences.visible()
        {
            preferences.close();
        }
    }
    fn toggle_preferences(&mut self) {
        if self.capturing_keyboard {
            return;
        }
        if let Some(preferences) = self.preferences.take()
            && preferences.visible()
        {
            preferences.close();
            return;
        }
        match Preferences::new(
            &self.saved,
            &self.models,
            &self.microphones,
            &self.monitors,
            &self.message,
            self.tx.clone(),
        ) {
            Ok(preferences) => {
                preferences.present();
                self.preferences = Some(preferences);
            }
            Err(error) => {
                self.message = format!("No se pudieron abrir los ajustes GTK4: {error}");
                eprintln!("{}", self.message);
            }
        }
    }
    pub fn catalog_loaded(&mut self) {
        if let Some(preferences) = &self.preferences {
            preferences.catalog_loaded(&self.models);
        }
        self.message = format!(
            "{} rutas disponibles · selecciona un modelo compatible con audio",
            self.models.len()
        );
        self.dirty = true;
    }
    fn find_output(&self, monitor_name: &str) -> Option<&wl_output::WlOutput> {
        if monitor_name.is_empty() {
            return None;
        }
        self.outputs
            .iter()
            .find(|o| o.name == monitor_name)
            .map(|o| &o.output)
    }
    fn update_max_x(&mut self) {
        let width = if self.draft.monitor.is_empty() {
            self.monitors.iter().find(|m| m.focused).map(|m| m.width)
        } else {
            self.monitors
                .iter()
                .find(|m| m.name == self.draft.monitor)
                .map(|m| m.width)
        }
        .unwrap_or(1280);
        self.max_x = width.saturating_sub(176);
    }
    fn setup_dock(&mut self, qh: &QueueHandle<Self>) {
        if let Some(layer) = self.layer.take() {
            layer.destroy();
        }
        if let Some(surface) = self.surface.take() {
            surface.destroy();
        }
        self.configured = false;
        let surface = self.compositor.as_ref().unwrap().create_surface(qh, ());
        let target_output = self.find_output(&self.draft.monitor);
        let layer = self.shell.as_ref().unwrap().get_layer_surface(
            &surface,
            target_output,
            Layer::Overlay,
            "hypr-dictate".into(),
            qh,
            (),
        );
        layer.set_size(176, 44);
        layer.set_anchor(Anchor::Bottom | Anchor::Left);
        self.update_max_x();
        self.draft.x = self.draft.x.clamp(0, self.max_x);
        layer.set_margin(0, 0, 0, self.draft.x);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.set_exclusive_zone(0);
        surface.commit();
        self.surface = Some(surface);
        self.layer = Some(layer);
        self.dirty = true;
    }
    pub fn is_visible(&self) -> bool {
        self.layer.is_some()
    }
    pub fn arm(&mut self, address: String, dock_was_hidden: bool) {
        self.close_preferences();
        self.arm_token = self.arm_token.wrapping_add(1);
        self.seen_enter = false;
        self.seen_modifiers = false;
        self.pressed_keys.clear();
        self.mods_depressed = 0;
        self.arming = true;
        self.arm_target = Some((
            self.arm_token,
            address,
            dock_was_hidden,
            std::time::Instant::now(),
        ));
        self.capture_keyboard(true);
    }
    pub fn check_arm_ready(&mut self) {
        if self.arming
            && self.seen_enter
            && self.seen_modifiers
            && self.mods_depressed == 0
            && self.pressed_keys.is_empty()
        {
            self.arming = false;
            if let Some((token, address, dock_was_hidden, _)) = self.arm_target.take() {
                let _ = self.tx.send(Event::ArmReady(token, address, dock_was_hidden));
            }
        }
    }
    pub fn cancel_arm(&mut self) {
        if self.arming {
            self.arm_token = self.arm_token.wrapping_add(1);
            self.arming = false;
            self.seen_enter = false;
            self.seen_modifiers = false;
            self.capture_keyboard(false);
            if let Some((_, address, dock_was_hidden, _)) = self.arm_target.take() {
                let _ = crate::restore(&address);
                if dock_was_hidden {
                    self.hide();
                }
            }
            self.message = "Atajo cancelado".into();
            self.dirty = true;
        }
    }
    pub fn capture_keyboard(&mut self, capture: bool) {
        self.capturing_keyboard = capture;
        if let Some(layer) = &self.layer {
            let interactivity = if capture {
                KeyboardInteractivity::Exclusive
            } else {
                KeyboardInteractivity::None
            };
            layer.set_keyboard_interactivity(interactivity);
            if let Some(surface) = &self.surface {
                surface.commit();
            }
            let _ = self.connection.flush();
        }
    }
    pub fn hide(&mut self) {
        self.capture_keyboard(false);
        self.close_preferences();
        if let Some(layer) = self.layer.take() {
            layer.destroy();
        }
        if let Some(surface) = self.surface.take() {
            surface.destroy();
        }
        self.configured = false;
        self.dirty = false;
        self.press = None;
        self.dragged = false;
        let _ = self.connection.flush();
    }
    pub fn show(&mut self) {
        if self.layer.is_none() {
            self.setup_dock(&self.qh.clone());
            let _ = self.connection.flush();
        }
    }
    pub fn toggle_visibility(&mut self) {
        if self.is_visible() {
            self.hide();
        } else {
            self.show();
        }
    }
    pub fn type_text(&mut self, text: &str, address: &str) -> Result<()> {
        let chunks = hypr_dictate::plan_chunks(text);
        if chunks.is_empty() {
            return Ok(());
        }
        // Wait for physical modifiers (CTRL/ALT/SHIFT/SUPER) to be released so the target app
        // never sees Ctrl+key or Alt+key combinations while typing.
        for _ in 0..100 {
            if self.mods_depressed == 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(15));
        }

        let manager = self.manager.as_ref().context("Falta virtual keyboard manager")?;
        let seat = self.seat.as_ref().context("Falta seat Wayland")?;

        for chunk in chunks {
            ensure!(
                crate::active_address()? == address,
                "Cambió el foco durante la inserción; inserción detenida"
            );

            // Crear un dispositivo de teclado virtual dedicado por chunk.
            // Esto asegura que el compositor (Hyprland) asigne y transmita un nuevo
            // descriptor de keymap a la superficie enfocada para cada bloque,
            // evitando que clientes Wayland retengan el mapa de teclas anterior.
            let keyboard = manager.create_virtual_keyboard(seat, &self.qh, ());

            let chunk_result = (|| -> Result<()> {
                let mut file = tempfile::tempfile()?;
                file.write_all(chunk.keymap.as_bytes())?;
                file.seek(SeekFrom::Start(0))?;
                keyboard.keymap(1, file.as_fd(), chunk.keymap.len() as u32);
                keyboard.modifiers(0, 0, 0, 0);
                self.connection.flush()?;
                std::thread::sleep(Duration::from_millis(6));

                for code in chunk.keycodes {
                    ensure!(
                        crate::active_address()? == address,
                        "Cambió el foco durante la inserción; inserción detenida"
                    );
                    keyboard.key(0, code, 1);
                    keyboard.key(0, code, 0);
                }
                self.connection.flush()?;
                std::thread::sleep(Duration::from_millis(8));
                Ok(())
            })();

            // Limpieza incondicional ante cualquier error o salida anticipada
            keyboard.modifiers(0, 0, 0, 0);
            keyboard.destroy();
            let _ = self.connection.flush();
            std::thread::sleep(Duration::from_millis(2));

            chunk_result?;
        }
        Ok(())
    }
}
impl Dispatch<wl_registry::WlRegistry, ()> for Ui {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_compositor" => {
                    state.compositor = Some(registry.bind(name, version.min(4), qh, ()))
                }
                "wl_shm" => state.shm = Some(registry.bind(name, 1, qh, ())),
                "wl_seat" if state.seat.is_none() => {
                    state.seat = Some(registry.bind(name, version.min(7), qh, ()))
                }
                "zwlr_layer_shell_v1" => {
                    state.shell = Some(registry.bind(name, version.min(4), qh, ()))
                }
                "wl_output" => {
                    let output: wl_output::WlOutput = registry.bind(name, version.min(4), qh, ());
                    state.outputs.push(OutputItem {
                        name: String::new(),
                        output,
                    });
                }
                "zwp_virtual_keyboard_manager_v1" => {
                    state.manager = Some(registry.bind(name, 1, qh, ()))
                }
                _ => {}
            }
        }
    }
}
impl Dispatch<wl_seat::WlSeat, ()> for Ui {
    fn event(
        state: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: wayland_client::WEnum::Value(caps),
        } = event
        {
            if caps.contains(wl_seat::Capability::Pointer) && state.pointer.is_none() {
                state.pointer = Some(seat.get_pointer(qh, ()));
            }
            if caps.contains(wl_seat::Capability::Keyboard) && state.seat_keyboard.is_none() {
                state.seat_keyboard = Some(seat.get_keyboard(qh, ()));
            }
        }
    }
}
impl Dispatch<wl_keyboard::WlKeyboard, ()> for Ui {
    fn event(
        state: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        use wayland_client::WEnum::Value;
        match event {
            wl_keyboard::Event::Keymap { format: _, fd, size } => {
                let keymap = unsafe {
                    xkb::Keymap::new_from_fd(
                        &state.xkb_context,
                        fd,
                        size as usize,
                        xkb::KEYMAP_FORMAT_TEXT_V1,
                        xkb::KEYMAP_COMPILE_NO_FLAGS,
                    )
                };
                if let Ok(Some(keymap)) = keymap {
                    state.xkb_state = Some(xkb::State::new(&keymap));
                }
            }
            wl_keyboard::Event::Enter { surface, keys, .. } => {
                if state.surface.as_ref() == Some(&surface) {
                    state.seen_enter = true;
                    state.pressed_keys.clear();
                    for chunk in keys.as_chunks::<4>().0 {
                        state.pressed_keys.insert(u32::from_ne_bytes(*chunk));
                    }
                    state.check_arm_ready();
                }
            }
            wl_keyboard::Event::Leave { surface, .. } => {
                if state.surface.as_ref() == Some(&surface) {
                    state.pressed_keys.clear();
                    state.mods_depressed = 0;
                    state.seen_enter = false;
                    state.seen_modifiers = false;
                }
            }
            wl_keyboard::Event::Modifiers {
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
                ..
            } => {
                state.mods_depressed = mods_depressed;
                if let Some(xkb_state) = &mut state.xkb_state {
                    xkb_state.update_mask(mods_depressed, mods_latched, mods_locked, 0, 0, group);
                }
                if state.seen_enter {
                    state.seen_modifiers = true;
                    state.check_arm_ready();
                }
            }
            wl_keyboard::Event::Key {
                key,
                state: Value(keystate),
                ..
            } if state.capturing_keyboard => {
                match keystate {
                    wl_keyboard::KeyState::Pressed => {
                        state.pressed_keys.insert(key);
                    }
                    wl_keyboard::KeyState::Released => {
                        state.pressed_keys.remove(&key);
                    }
                    _ => {}
                }
                if state.arming {
                    let xkb_key = xkb::Keycode::new(key + 8);
                    let sym = state.xkb_state.as_ref().map(|s| s.key_get_one_sym(xkb_key));
                    if sym == Some(xkb::Keysym::Escape) && keystate == wl_keyboard::KeyState::Pressed {
                        state.cancel_arm();
                        return;
                    }
                    state.check_arm_ready();
                    return;
                }
                let xkb_key = xkb::Keycode::new(key + 8);
                let sym = state.xkb_state.as_ref().map(|s| s.key_get_one_sym(xkb_key));
                let capture_key = match sym {
                    Some(xkb::Keysym::Return) => Some(hypr_dictate::CaptureKey::Return),
                    Some(xkb::Keysym::KP_Enter) => Some(hypr_dictate::CaptureKey::KpEnter),
                    Some(xkb::Keysym::Escape) => Some(hypr_dictate::CaptureKey::Escape),
                    _ => None,
                };
                if let Some(ck) = capture_key {
                    match keystate {
                        wl_keyboard::KeyState::Pressed => {
                            let _ = state.tx.send(Event::KeyPress(ck));
                        }
                        wl_keyboard::KeyState::Released => {
                            let _ = state.tx.send(Event::KeyRelease(ck));
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        if state.dirty && let Err(error) = state.draw(qh) {
            state.message = error.to_string();
        }
    }
}
impl Dispatch<wl_pointer::WlPointer, ()> for Ui {
    fn event(
        state: &mut Self,
        _: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        use wayland_client::WEnum::Value;
        match event {
            wl_pointer::Event::Enter {
                surface_x,
                surface_y,
                ..
            } => {
                state.pointer_xy = (surface_x, surface_y);
            }
            wl_pointer::Event::Leave { .. } => {
                state.press = None;
            }
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => {
                state.pointer_xy = (surface_x, surface_y);
                if let Some((x, _, origin)) = state.press {
                    let delta = surface_x - x;
                    let origin = if state.dragged { state.draft.x } else { origin };
                    if delta.abs() > 6. || state.dragged {
                        state.dragged = true;
                        state.draft.x = (origin + delta as i32).clamp(0, state.max_x);
                        if let Some(layer) = &state.layer {
                            layer.set_margin(0, 0, 0, state.draft.x);
                        }
                        if let Some(surface) = &state.surface {
                            surface.commit();
                        }
                    }
                }
            }
            wl_pointer::Event::Button {
                button: 273,
                state: Value(wl_pointer::ButtonState::Pressed),
                ..
            } => {
                if !state.capturing_keyboard {
                    state.toggle_preferences();
                }
            }
            wl_pointer::Event::Button {
                button: 272,
                state: Value(wl_pointer::ButtonState::Pressed),
                ..
            } => {
                state.press = Some((state.pointer_xy.0, state.pointer_xy.1, state.draft.x));
                state.dragged = false;
            }
            wl_pointer::Event::Button {
                button: 272,
                state: Value(wl_pointer::ButtonState::Released),
                ..
            } if state.press.take().is_some() => {
                if state.dragged {
                    state.save_position();
                } else {
                    let _ = state.tx.send(Event::Command("toggle".into()));
                }
            }
            _ => {}
        }
        if state.dirty && let Err(error) = state.draw(qh) {
            state.message = error.to_string();
        }
    }
}
impl Dispatch<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, ()> for Ui {
    fn event(
        state: &mut Self,
        layer: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure { serial, .. } => {
                if state.layer.as_ref() != Some(layer) {
                    return;
                }
                layer.ack_configure(serial);
                state.configured = true;
                state.dirty = true;
                if let Err(error) = state.draw(qh) {
                    eprintln!("No se pudo dibujar: {error}");
                    state.closed = true;
                }
            }
            zwlr_layer_surface_v1::Event::Closed
                if state.layer.as_ref() == Some(layer) =>
            {
                state.closed = true;
            }
            _ => {}
        }
    }
}
impl Dispatch<wl_buffer::WlBuffer, ()> for Ui {
    fn event(
        state: &mut Self,
        buffer: &wl_buffer::WlBuffer,
        event: wl_buffer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = event {
            state.buffers = state.buffers.saturating_sub(1);
            buffer.destroy();
        }
    }
}
impl Dispatch<wl_output::WlOutput, ()> for Ui {
    fn event(
        state: &mut Self,
        output: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Name { name } = event
            && let Some(item) = state.outputs.iter_mut().find(|o| &o.output == output)
        {
            item.name = name;
        }
    }
}
delegate_noop!(Ui: ignore wl_compositor::WlCompositor);
delegate_noop!(Ui: ignore wl_surface::WlSurface);
delegate_noop!(Ui: ignore wl_shm::WlShm);
delegate_noop!(Ui: ignore wl_shm_pool::WlShmPool);
delegate_noop!(Ui: ignore zwlr_layer_shell_v1::ZwlrLayerShellV1);
delegate_noop!(Ui: ignore zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1);
delegate_noop!(Ui: ignore zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1);

fn monitor_width() -> Option<i32> {
    let value: serde_json::Value =
        serde_json::from_slice(&crate::hypr(&["-j", "monitors"]).ok()?).ok()?;
    let monitor = value.as_array()?.iter().find(|m| m["focused"] == true)?;
    Some((monitor["width"].as_f64()? / monitor["scale"].as_f64().unwrap_or(1.)) as i32)
}
pub fn run(mut app: App, rx: mpsc::Receiver<Event>) -> Result<()> {
    let connection = Connection::connect_to_env().context("No se pudo conectar con Wayland")?;
    let mut queue = connection.new_event_queue();
    let qh = queue.handle();
    // Microphone detection is a cheap local pw-dump call, unlike the model
    // catalog which needs a network round trip, so it is resolved up front
    // and never blocks startup if PipeWire's CLI tooling has an issue.
    let microphones = crate::settings::microphones().unwrap_or_default();
    let monitors = crate::settings::monitors();
    let mut ui = Ui::new(
        connection.clone(),
        qh.clone(),
        &app,
        microphones,
        monitors,
        monitor_width().unwrap_or(1280).saturating_sub(176),
    );
    connection.display().get_registry(&qh, ());
    queue.roundtrip(&mut ui)?;
    queue.roundtrip(&mut ui)?;
    ensure!(
        ui.compositor.is_some()
            && ui.shm.is_some()
            && ui.seat.is_some()
            && ui.shell.is_some()
            && ui.manager.is_some(),
        "El compositor no ofrece los protocolos Wayland necesarios"
    );
    ui.keyboard = Some(ui.manager.as_ref().unwrap().create_virtual_keyboard(
        ui.seat.as_ref().unwrap(),
        &qh,
        (),
    ));
    ui.setup_dock(&qh);
    queue.roundtrip(&mut ui)?;
    while !ui.closed {
        queue.dispatch_pending(&mut ui)?;
        let context = gtk4::glib::MainContext::default();
        for _ in 0..64 {
            if !context.pending() {
                break;
            }
            context.iteration(false);
        }
        if ui.preferences.as_ref().is_some_and(|p| !p.visible()) {
            ui.preferences = None;
        }
        while let Ok(event) = rx.try_recv() {
            app.event(event, &mut ui);
            if ui.is_visible() {
                ui.dirty = true;
            }
        }
        app.tick(&mut ui);
        if ui.dock_reconfigure {
            ui.setup_dock(&qh);
            ui.dock_reconfigure = false;
        }
        let color = app.color();
        let active = matches!(
            app.phase,
            hypr_dictate::Phase::Recording(_) | hypr_dictate::Phase::Processing(_)
        );
        let target = if active { 1. } else { 0. };
        if (ui.open_progress - target).abs() > 0.01 {
            ui.open_progress = (ui.open_progress
                + if target > ui.open_progress {
                    0.14
                } else {
                    -0.1
                })
            .clamp(0., 1.);
            ui.dirty = true;
        }
        let animating = active || ui.open_progress > 0.01;
        if matches!(app.phase, hypr_dictate::Phase::Processing(_)) {
            ui.level = -1.;
        } else {
            let peak = app.level.load(std::sync::atomic::Ordering::Relaxed) as f64;
            // Clean noise floor filter: ignore quiet room ambient noise under 100
            // and use square root scaling for vibrant, responsive voice dynamics
            let normalized = ((peak - 100.).max(0.) / 2400.).min(1.);
            let raw = normalized.sqrt();
            if raw > ui.level {
                ui.level = raw;
            } else {
                ui.level = (ui.level * 0.86 - 0.008).max(0.);
            }
        }
        if animating || color != ui.color {
            ui.animate_dock(color);
        }
        if ui.dirty && ui.configured {
            ui.draw(&qh)?;
        }
        connection.flush()?;
        if let Some(guard) = queue.prepare_read() {
            let mut fd = libc::pollfd {
                fd: std::os::fd::AsRawFd::as_raw_fd(&connection.backend().poll_fd()),
                events: libc::POLLIN,
                revents: 0,
            };
            let timeout = if animating || ui.preferences.is_some() {
                8
            } else {
                50
            };
            let ready = unsafe { libc::poll(&mut fd, 1, timeout) };
            if ready > 0 {
                if let Err(error) = guard.read()
                    && !hypr_dictate::transient_wayland_error(&error)
                {
                    return Err(error.into());
                }
            } else {
                drop(guard);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{Microphone, MonitorInfo};

    fn ui() -> (Ui, std::os::unix::net::UnixStream) {
        let (socket, server) = std::os::unix::net::UnixStream::pair().unwrap();
        let connection = Connection::from_socket(socket).unwrap();
        let queue = connection.new_event_queue::<Ui>();
        let qh = queue.handle();
        let mut app = crate::tests::app(hypr_dictate::Phase::Idle);
        app.settings = crate::settings::Settings {
            model: "account/model".into(),
            shortcut: "SUPER + F9".into(),
            microphone: "saved-mic".into(),
            monitor: "saved-monitor".into(),
            ..Default::default()
        };
        let mut ui = Ui::new(
            connection,
            qh,
            &app,
            vec![Microphone {
                name: "saved-mic".into(),
                description: "Mic".into(),
            }],
            vec![MonitorInfo {
                name: "saved-monitor".into(),
                description: "Monitor".into(),
                width: 1920,
                focused: true,
            }],
            1744,
        );
        ui.models = vec!["account/model".into()];
        (ui, server)
    }

    #[test]
    fn dragging_emits_only_position_even_without_catalog() {
        let (mut ui, _server) = ui();
        let (tx, rx) = mpsc::channel();
        ui.tx = tx;
        ui.models.clear();
        ui.draft.x = 456;
        ui.draft.shortcut = "CTRL + ALT + D".into();
        ui.draft.model = "unsaved/model".into();
        ui.draft.prompt = "unsaved prompt".into();
        let draft = format!("{:?}", ui.draft);
        ui.save_position();
        assert!(matches!(rx.try_recv().unwrap(), Event::SavePosition(456)));
        assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
        assert_eq!(format!("{:?}", ui.draft), draft);
    }

    #[test]
    fn stale_catalog_success_and_failure_do_not_change_ui_or_phase() {
        let (mut ui, _server) = ui();
        let mut app = crate::tests::app(hypr_dictate::Phase::Idle);
        app.catalog_generation = 3;
        let models = ui.models.clone();
        let message = ui.message.clone();
        for result in [
            Ok(vec!["old/model".into()]),
            Err(anyhow::anyhow!("old failure")),
        ] {
            app.event(
                Event::Catalog(app.settings.base_url.clone(), 1, result),
                &mut ui,
            );
            assert_eq!(ui.models, models);
            assert_eq!(ui.message, message);
            assert_eq!(app.phase, hypr_dictate::Phase::Idle);
        }
        app.event(
            Event::Catalog(
                app.settings.base_url.clone(),
                3,
                Ok(vec!["new/model".into()]),
            ),
            &mut ui,
        );
        assert_eq!(ui.models, ["new/model"]);
    }

    #[test]
    fn settings_apply_reconfigures_dock_only_after_success_and_preserves_server() {
        let (mut ui, _server) = ui();
        let mut settings = ui.saved.clone();
        settings.monitor = "other".into();
        ui.settings_saved(&settings);
        assert!(ui.dock_reconfigure);
        assert_eq!(ui.saved.monitor, "other");
        ui.server_saved("https://new.example");
        assert_eq!(ui.saved.monitor, "other");
        assert_eq!(ui.saved.base_url, "https://new.example");
        assert!(ui.models.is_empty());
    }

    #[test]
    #[ignore = "requiere receptor Wayland dedicado: HYPR_DICTATE_TEST_ADDRESS y HYPR_DICTATE_TEST_TITLE"]
    fn test_type_text_into_isolated_receiver() {
        let address = std::env::var("HYPR_DICTATE_TEST_ADDRESS")
            .expect("HYPR_DICTATE_TEST_ADDRESS ausente");
        let address = address.trim();
        assert!(!address.is_empty(), "HYPR_DICTATE_TEST_ADDRESS vacío");

        let expected_title = std::env::var("HYPR_DICTATE_TEST_TITLE")
            .expect("HYPR_DICTATE_TEST_TITLE ausente");
        let expected_title = expected_title.trim();
        assert!(!expected_title.is_empty(), "HYPR_DICTATE_TEST_TITLE vacío");

        // Validar destino existente y título exacto contra Hyprland
        let clients_bytes = crate::hypr(&["-j", "clients"]).expect("hyprctl clients");
        let clients: serde_json::Value =
            serde_json::from_slice(&clients_bytes).expect("JSON clients válido");
        let client = clients
            .as_array()
            .and_then(|arr| arr.iter().find(|c| c["address"] == address))
            .expect("Ventana de prueba no encontrada en Hyprland");

        let actual_title = client["title"].as_str().unwrap_or("");
        assert_eq!(
            actual_title, expected_title,
            "El título de la ventana no coincide con el receptor esperado"
        );

        // Validar foco activo
        let active_bytes = crate::hypr(&["-j", "activewindow"]).expect("hyprctl activewindow");
        let active: serde_json::Value =
            serde_json::from_slice(&active_bytes).expect("JSON activewindow válido");
        assert_eq!(
            active["address"].as_str().unwrap_or(""),
            address,
            "La ventana receptora debe tener el foco activo antes de iniciar el test"
        );

        let connection = Connection::connect_to_env().expect("Conexión Wayland");
        let mut queue = connection.new_event_queue();
        let qh = queue.handle();
        let (tx, _rx) = mpsc::channel();
        let mut ui = Ui::new_for_test(connection.clone(), qh.clone(), tx);
        connection.display().get_registry(&qh, ());
        queue.roundtrip(&mut ui).unwrap();
        queue.roundtrip(&mut ui).unwrap();
        if let (Some(manager), Some(seat)) = (&ui.manager, &ui.seat) {
            ui.keyboard = Some(manager.create_virtual_keyboard(seat, &qh, ()));
        } else {
            panic!("El compositor no ofrece zwp_virtual_keyboard_manager_v1 o wl_seat");
        }
        queue.roundtrip(&mut ui).unwrap();

        let phrase = "Genial, funciona muy bien. Está perfecto, así que bueno, esto es una transcripción de prueba.";
        ui.type_text(phrase, address).expect("Ui::type_text original completó sin error");
    }
}
