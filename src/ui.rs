use crate::{App, Event, paint::Modal};
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
        wl_buffer, wl_compositor, wl_keyboard, wl_output, wl_pointer, wl_registry, wl_seat, wl_shm, wl_shm_pool, wl_surface,
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

const SHORTCUTS: [&str; 7] = [
    "",
    "SUPER + ALT + V",
    "CTRL + SHIFT + D",
    "CTRL + ALT + D",
    "SUPER + SHIFT + D",
    "SUPER + F9",
    "SUPER + F10",
];

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
    modal_surface: Option<wl_surface::WlSurface>,
    modal_layer: Option<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1>,
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
    configured: bool,
    modal_configured: bool,
    closed: bool,
    color: [u8; 3],
    pub models: Vec<String>,
    pub message: String,
    draft: crate::settings::Settings,
    panel: bool,
    open_select: Option<usize>,
    in_modal: bool,
    pointer_xy: (f64, f64),
    press: Option<(f64, f64, i32)>,
    dragged: bool,
    tick: f64,
    open_progress: f64,
    pub level: f64,
    account_index: usize,
    model_index: usize,
    shortcut_index: usize,
    pub microphones: Vec<crate::settings::Microphone>,
    microphone_index: usize,
    pub monitors: Vec<crate::settings::MonitorInfo>,
    outputs: Vec<OutputItem>,
    monitor_index: usize,
    buffers: usize,
    pub dirty: bool,
    max_x: i32,
    pub tab: usize,
    pub prompt_cursor: usize,
    pub prompt_focused: bool,
    seat_keyboard: Option<wl_keyboard::WlKeyboard>,
    xkb_context: xkb::Context,
    xkb_state: Option<xkb::State>,
}
impl Ui {
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
        if self.panel && self.modal_configured {
            let accounts = self.accounts();
            let account = &accounts[self.account_index.min(accounts.len() - 1)];
            let shortcut_labels: Vec<String> = SHORTCUTS
                .iter()
                .map(|s| {
                    if s.is_empty() {
                        "Sin atajo".into()
                    } else {
                        (*s).into()
                    }
                })
                .collect();
            let mic_labels = self.microphone_labels();
            let mon_labels = self.monitor_labels();
            let (options, selected) = match self.open_select {
                Some(0) => (accounts.clone(), self.account_index),
                Some(1) => (self.available(), self.model_index),
                Some(2) => (shortcut_labels.clone(), self.shortcut_index),
                Some(3) => (mic_labels.clone(), self.microphone_index),
                Some(4) => (mon_labels.clone(), self.monitor_index),
                _ => (Vec::new(), 0),
            };
            let pixels = crate::paint::modal(
                540,
                crate::paint::MODAL_HEIGHT,
                self.color,
                &Modal {
                    tab: self.tab,
                    account,
                    model: &self.draft.model,
                    shortcut: &shortcut_labels[self.shortcut_index],
                    microphone: &mic_labels[self.microphone_index.min(mic_labels.len() - 1)],
                    monitor: &mon_labels[self.monitor_index.min(mon_labels.len() - 1)],
                    open: self.open_select,
                    options: &options,
                    selected,
                    message: &self.message,
                    temperature: self.draft.temperature,
                    prompt: &self.draft.prompt,
                    prompt_cursor: self.prompt_cursor,
                    prompt_focused: self.prompt_focused,
                },
            )?;
            let surface = self.modal_surface.as_ref().unwrap().clone();
            self.attach(&surface, 540, crate::paint::MODAL_HEIGHT, &pixels, qh)?;
        }
        Ok(())
    }
    fn toggle_modal(&mut self, qh: &QueueHandle<Self>) {
        if self.capturing_keyboard {
            return;
        }
        self.capture_keyboard(false);
        if self.panel {
            self.panel = false;
            self.open_select = None;
            self.prompt_focused = false;
            if let Some(layer) = self.modal_layer.take() {
                layer.destroy();
            }
            if let Some(surface) = self.modal_surface.take() {
                surface.destroy();
            }
            self.modal_configured = false;
        } else {
            self.panel = true;
            self.tab = 0;
            self.prompt_cursor = self.draft.prompt.chars().count();
            self.prompt_focused = false;
            let surface = self.compositor.as_ref().unwrap().create_surface(qh, ());
            let layer = self.shell.as_ref().unwrap().get_layer_surface(
                &surface,
                None,
                Layer::Overlay,
                "hypr-dictate-modal".into(),
                qh,
                (),
            );
            layer.set_size(540, crate::paint::MODAL_HEIGHT as u32);
            layer.set_keyboard_interactivity(KeyboardInteractivity::OnDemand);
            surface.commit();
            self.modal_surface = Some(surface);
            self.modal_layer = Some(layer);
        }
        self.dirty = true;
    }
    pub fn catalog_loaded(&mut self) {
        let account = self
            .draft
            .model
            .split_once('/')
            .map(|(a, _)| a.to_string())
            .unwrap_or_else(|| "Automática".into());
        self.account_index = self
            .accounts()
            .iter()
            .position(|a| a == &account)
            .unwrap_or(0);
        self.model_index = self
            .available()
            .iter()
            .position(|m| m == &self.draft.model)
            .unwrap_or(0);
        self.dirty = true;
        self.message = format!(
            "{} rutas disponibles · elige cuenta, modelo, atajo, micro y pantalla",
            self.models.len()
        );
    }
    fn accounts(&self) -> Vec<String> {
        let mut out = vec!["Automática".into()];
        for model in &self.models {
            if let Some((account, _)) = model.split_once('/')
                && !out.iter().any(|a| a == account)
            {
                out.push(account.into());
            }
        }
        out
    }
    fn available(&self) -> Vec<String> {
        let accounts = self.accounts();
        let account = &accounts[self.account_index.min(accounts.len() - 1)];
        self.models
            .iter()
            .filter(|m| {
                if account == "Automática" {
                    !m.contains('/')
                } else {
                    m.starts_with(&format!("{account}/"))
                }
            })
            .cloned()
            .collect()
    }
    fn choose_model(&mut self, step: isize) {
        let models = self.available();
        if !models.is_empty() {
            self.model_index =
                (self.model_index as isize + step).rem_euclid(models.len() as isize) as usize;
            self.draft.model = models[self.model_index].clone();
        }
    }
    fn microphone_labels(&self) -> Vec<String> {
        let mut out = vec!["Automático".into()];
        out.extend(self.microphones.iter().map(|m| m.description.clone()));
        out
    }
    fn choose_microphone(&mut self, step: isize) {
        let len = self.microphones.len() + 1;
        self.microphone_index = (self.microphone_index as isize + step).rem_euclid(len as isize) as usize;
        self.draft.microphone = if self.microphone_index == 0 {
            String::new()
        } else {
            self.microphones[self.microphone_index - 1].name.clone()
        };
    }
    fn monitor_labels(&self) -> Vec<String> {
        let mut out = vec!["Automática (enfocada)".into()];
        out.extend(self.monitors.iter().map(|m| {
            if m.description.is_empty() || m.description == m.name {
                m.name.clone()
            } else {
                format!("{} ({})", m.name, m.description)
            }
        }));
        out
    }
    fn find_output(&self, monitor_name: &str) -> Option<&wl_output::WlOutput> {
        if monitor_name.is_empty() {
            return None;
        }
        self.outputs.iter().find(|o| o.name == monitor_name).map(|o| &o.output)
    }
    fn update_max_x(&mut self) {
        let width = if self.draft.monitor.is_empty() {
            self.monitors.iter().find(|m| m.focused).map(|m| m.width)
        } else {
            self.monitors.iter().find(|m| m.name == self.draft.monitor).map(|m| m.width)
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
    pub fn close_modal(&mut self) {
        if self.panel {
            self.panel = false;
            self.open_select = None;
            self.prompt_focused = false;
            if let Some(layer) = self.modal_layer.take() {
                layer.destroy();
            }
            if let Some(surface) = self.modal_surface.take() {
                surface.destroy();
            }
            self.modal_configured = false;
            self.dirty = true;
            let _ = self.connection.flush();
        }
    }
    pub fn is_visible(&self) -> bool {
        self.layer.is_some()
    }
    pub fn arm(&mut self, address: String, dock_was_hidden: bool) {
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
        if self.panel {
            self.toggle_modal(&self.qh.clone());
        } else if let Some(layer) = self.modal_layer.take() {
            layer.destroy();
            if let Some(surface) = self.modal_surface.take() {
                surface.destroy();
            }
            self.modal_configured = false;
        }
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
    fn choose_monitor(&mut self, step: isize, qh: &QueueHandle<Self>) {
        let len = self.monitors.len() + 1;
        self.monitor_index = (self.monitor_index as isize + step).rem_euclid(len as isize) as usize;
        self.draft.monitor = if self.monitor_index == 0 {
            String::new()
        } else {
            self.monitors[self.monitor_index - 1].name.clone()
        };
        self.setup_dock(qh);
    }
    fn modal_click(&mut self, qh: &QueueHandle<Self>) {
        let (x, y) = self.pointer_xy;

        // Header buttons: Reload and Close
        if (12.0..38.0).contains(&y) {
            if (345.0..440.0).contains(&x) {
                let _ = self.tx.send(Event::Command("reload".into()));
                self.message = "Dock reiniciado".into();
                self.dirty = true;
                return;
            } else if x > 445.0 {
                self.toggle_modal(qh);
                return;
            }
        }

        // Tab selection buttons
        if (40.0..76.0).contains(&y) {
            if (26.0..130.0).contains(&x) {
                self.tab = 0;
                self.open_select = None;
                self.prompt_focused = false;
                self.dirty = true;
                return;
            } else if (132.0..280.0).contains(&x) {
                self.tab = 1;
                self.open_select = None;
                self.dirty = true;
                return;
            }
        }

        let actions_y = 412.;

        if self.tab == 0 {
            if let Some(idx) = self.open_select {
                let count = match idx {
                    0 => self.accounts().len(),
                    1 => self.available().len(),
                    2 => SHORTCUTS.len(),
                    3 => self.microphones.len() + 1,
                    _ => self.monitors.len() + 1,
                };
                let visible = count.min(6);
                let h_list = visible as f64 * 30. + 10.;
                let base_y = if idx == 4 {
                    crate::paint::ROW_TOP[idx] - h_list + 6.
                } else {
                    crate::paint::ROW_TOP[idx] + 48.
                };
                let start = [
                    self.account_index,
                    self.model_index,
                    self.shortcut_index,
                    self.microphone_index,
                    self.monitor_index,
                ][idx]
                    .saturating_sub(2)
                    .min(count.saturating_sub(visible));
                if (26.0..514.0).contains(&x)
                    && (base_y..base_y + visible as f64 * 30. + 10.).contains(&y)
                {
                    let chosen = (start + ((y - base_y - 5.).max(0.) as usize / 30))
                        .min(count.saturating_sub(1));
                    match idx {
                        0 => {
                            self.account_index = chosen;
                            self.model_index = 0;
                            self.choose_model(0);
                        }
                        1 => {
                            self.model_index = chosen;
                            self.choose_model(0);
                        }
                        2 => {
                            self.shortcut_index = chosen;
                            self.draft.shortcut = SHORTCUTS[chosen].into();
                        }
                        3 => {
                            self.microphone_index = chosen;
                            self.choose_microphone(0);
                        }
                        _ => {
                            self.monitor_index = chosen;
                            self.choose_monitor(0, qh);
                        }
                    }
                    self.open_select = None;
                    return;
                }
            }
            for (idx, top) in crate::paint::ROW_TOP.map(|t| t + 8.).into_iter().enumerate() {
                if (26.0..514.0).contains(&x) && (top..top + 38.).contains(&y) {
                    self.open_select = if self.open_select == Some(idx) {
                        None
                    } else {
                        Some(idx)
                    };
                    return;
                }
            }
            self.open_select = None;

            if (actions_y..actions_y + 40.).contains(&y) {
                if (26.0..206.0).contains(&x) {
                    let _ = self.tx.send(Event::Save(self.draft.clone()));
                } else if (216.0..326.0).contains(&x) {
                    self.toggle_modal(qh);
                }
            }
        } else {
            // Tab 1: Parámetros e IA
            // Click on Temperature slider
            if (crate::paint::TEMP_BOX.1..crate::paint::TEMP_BOX.1 + crate::paint::TEMP_BOX.3).contains(&y)
                && (crate::paint::TEMP_BOX.0..crate::paint::TEMP_BOX.0 + crate::paint::TEMP_BOX.2).contains(&x)
            {
                let fraction = (x - crate::paint::TEMP_BOX.0) / crate::paint::TEMP_BOX.2;
                self.draft.temperature = ((fraction * 20.0).round() * 0.05).clamp(0.0, 1.0);
                self.dirty = true;
                return;
            }

            // Click inside prompt box
            if (crate::paint::PROMPT_BOX.1..crate::paint::PROMPT_BOX.1 + crate::paint::PROMPT_BOX.3).contains(&y)
                && (crate::paint::PROMPT_BOX.0..crate::paint::PROMPT_BOX.0 + crate::paint::PROMPT_BOX.2).contains(&x)
            {
                self.prompt_focused = true;
                let line_idx = ((y - crate::paint::PROMPT_BOX.1 - 10.0).max(0.0) / 18.0) as usize;
                let lines = crate::paint::wrap_prompt_lines(&self.draft.prompt, 54);
                if !lines.is_empty() {
                    let clamped_line = line_idx.min(lines.len() - 1);
                    let (start_char, line_str) = &lines[clamped_line];
                    let approx_char = ((x - crate::paint::PROMPT_BOX.0 - 12.0).max(0.0) / 7.2) as usize;
                    let line_len = line_str.chars().count();
                    self.prompt_cursor = start_char + approx_char.min(line_len);
                }
                self.dirty = true;
                return;
            }

            // Unfocus prompt if click is outside prompt box
            self.prompt_focused = false;

            if (actions_y..actions_y + 40.).contains(&y) {
                if (26.0..176.0).contains(&x) {
                    let _ = self.tx.send(Event::Save(self.draft.clone()));
                } else if (186.0..326.0).contains(&x) {
                    self.draft.prompt = crate::settings::DEFAULT_PROMPT.into();
                    self.draft.temperature = crate::settings::DEFAULT_TEMPERATURE;
                    self.prompt_cursor = self.draft.prompt.chars().count();
                    self.message = "Parámetros restaurados a base".into();
                    self.dirty = true;
                } else if (336.0..426.0).contains(&x) {
                    self.toggle_modal(qh);
                }
            }
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
                state: Value(wl_keyboard::KeyState::Pressed),
                ..
            } if state.panel && state.tab == 1 && state.prompt_focused => {
                let xkb_key = xkb::Keycode::new(key + 8);
                if let Some(xkb_state) = &state.xkb_state {
                    let sym = xkb_state.key_get_one_sym(xkb_key);
                    let utf8 = xkb_state.key_get_utf8(xkb_key);

                    let prompt_chars: Vec<char> = state.draft.prompt.chars().collect();
                    let total_chars = prompt_chars.len();
                    state.prompt_cursor = state.prompt_cursor.min(total_chars);

                    match sym {
                        xkb::Keysym::BackSpace => {
                            if state.prompt_cursor > 0 {
                                state.prompt_cursor -= 1;
                                let mut new_prompt =
                                    String::with_capacity(state.draft.prompt.len());
                                for (i, ch) in prompt_chars.iter().enumerate() {
                                    if i != state.prompt_cursor {
                                        new_prompt.push(*ch);
                                    }
                                }
                                state.draft.prompt = new_prompt;
                                state.dirty = true;
                            }
                        }
                        xkb::Keysym::Delete => {
                            if state.prompt_cursor < total_chars {
                                let mut new_prompt =
                                    String::with_capacity(state.draft.prompt.len());
                                for (i, ch) in prompt_chars.iter().enumerate() {
                                    if i != state.prompt_cursor {
                                        new_prompt.push(*ch);
                                    }
                                }
                                state.draft.prompt = new_prompt;
                                state.dirty = true;
                            }
                        }
                        xkb::Keysym::Left => {
                            if state.prompt_cursor > 0 {
                                state.prompt_cursor -= 1;
                                state.dirty = true;
                            }
                        }
                        xkb::Keysym::Right => {
                            if state.prompt_cursor < total_chars {
                                state.prompt_cursor += 1;
                                state.dirty = true;
                            }
                        }
                        xkb::Keysym::Home => {
                            state.prompt_cursor = 0;
                            state.dirty = true;
                        }
                        xkb::Keysym::End => {
                            state.prompt_cursor = total_chars;
                            state.dirty = true;
                        }
                        xkb::Keysym::Return => {
                            let mut new_prompt =
                                String::with_capacity(state.draft.prompt.len() + 1);
                            for (i, ch) in prompt_chars.iter().enumerate() {
                                if i == state.prompt_cursor {
                                    new_prompt.push(' ');
                                }
                                new_prompt.push(*ch);
                            }
                            if state.prompt_cursor == total_chars {
                                new_prompt.push(' ');
                            }
                            state.prompt_cursor += 1;
                            state.draft.prompt = new_prompt;
                            state.dirty = true;
                        }
                        _ => {
                            if !utf8.is_empty() && !utf8.chars().any(char::is_control) {
                                let inserted_count = utf8.chars().count();
                                let mut new_prompt = String::with_capacity(
                                    state.draft.prompt.len() + utf8.len(),
                                );
                                let mut inserted = false;
                                for (i, ch) in prompt_chars.iter().enumerate() {
                                    if i == state.prompt_cursor {
                                        new_prompt.push_str(&utf8);
                                        inserted = true;
                                    }
                                    new_prompt.push(*ch);
                                }
                                if !inserted {
                                    new_prompt.push_str(&utf8);
                                }
                                state.prompt_cursor += inserted_count;
                                state.draft.prompt = new_prompt;
                                state.dirty = true;
                            }
                        }
                    }
                }
            }
            wl_keyboard::Event::Key {
                key,
                state: Value(keystate),
                ..
            } if state.capturing_keyboard && !state.panel => {
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
                surface,
                surface_x,
                surface_y,
                ..
            } => {
                state.in_modal = state.modal_surface.as_ref() == Some(&surface);
                state.pointer_xy = (surface_x, surface_y);
            }
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => {
                state.pointer_xy = (surface_x, surface_y);
                if !state.in_modal
                    && let Some((x, _, origin)) = state.press
                {
                    let delta = surface_x - x;
                    let origin = if state.dragged { state.draft.x } else { origin };
                    if delta.abs() > 6. || state.dragged {
                        state.dragged = true;
                        state.draft.x = (origin + delta as i32).clamp(0, state.max_x);
                        state
                            .layer
                            .as_ref()
                            .unwrap()
                            .set_margin(0, 0, 0, state.draft.x);
                        state.surface.as_ref().unwrap().commit();
                    }
                }
            }
            wl_pointer::Event::Axis { value, .. } if state.in_modal => {
                let step = if value > 0. { 1 } else { -1 };
                if state.tab == 1 {
                    if (crate::paint::TEMP_BOX.1 - 10.0..crate::paint::TEMP_BOX.1 + crate::paint::TEMP_BOX.3 + 10.0)
                        .contains(&state.pointer_xy.1)
                    {
                        let delta = if value > 0. { -0.05 } else { 0.05 };
                        state.draft.temperature =
                            ((state.draft.temperature + delta) * 100.0).round() / 100.0;
                        state.draft.temperature = state.draft.temperature.clamp(0.0, 1.0);
                        state.dirty = true;
                    }
                } else {
                    match state.open_select {
                        Some(0) => {
                            let len = state.accounts().len() as isize;
                            state.account_index =
                                (state.account_index as isize + step).rem_euclid(len) as usize;
                            state.model_index = 0;
                            state.choose_model(0);
                        }
                        Some(2) => {
                            state.shortcut_index = (state.shortcut_index as isize + step)
                                .rem_euclid(SHORTCUTS.len() as isize)
                                as usize;
                            state.draft.shortcut = SHORTCUTS[state.shortcut_index].into();
                        }
                        Some(3) => state.choose_microphone(step),
                        Some(4) => state.choose_monitor(step, qh),
                        _ => state.choose_model(step),
                    }
                    state.dirty = true;
                }
            }
            wl_pointer::Event::Button {
                button: 273,
                state: Value(wl_pointer::ButtonState::Pressed),
                ..
            } => {
                if !state.capturing_keyboard {
                    state.toggle_modal(qh);
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
                if state.in_modal {
                    state.modal_click(qh);
                } else if state.dragged {
                    let _ = state.tx.send(Event::Save(state.draft.clone()));
                } else {
                    let _ = state.tx.send(Event::Command("toggle".into()));
                }
                state.dirty = true;
            }
            _ => {}
        }
        if state.dirty
            && let Err(error) = state.draw(qh)
        {
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
                let is_modal = state.modal_layer.as_ref() == Some(layer);
                let is_dock = state.layer.as_ref() == Some(layer);
                if !is_modal && !is_dock {
                    return;
                }
                layer.ack_configure(serial);
                if is_modal {
                    state.modal_configured = true;
                } else {
                    state.configured = true;
                }
                state.dirty = true;
                if let Err(error) = state.draw(qh) {
                    eprintln!("No se pudo dibujar: {error}");
                    state.closed = true;
                }
            }
            zwlr_layer_surface_v1::Event::Closed => {
                if state.modal_layer.as_ref() == Some(layer) {
                    state.toggle_modal(qh);
                } else if state.layer.as_ref() == Some(layer) {
                    state.closed = true;
                }
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
    let mut ui = Ui {
        connection: connection.clone(),
        qh: qh.clone(),
        compositor: None,
        shm: None,
        seat: None,
        shell: None,
        manager: None,
        keyboard: None,
        surface: None,
        layer: None,
        modal_surface: None,
        modal_layer: None,
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
        configured: false,
        modal_configured: false,
        closed: false,
        color: app.color(),
        models: Vec::new(),
        message: "Cargando catálogo…".into(),
        draft: app.settings.clone(),
        panel: false,
        open_select: None,
        in_modal: false,
        pointer_xy: (0., 0.),
        press: None,
        dragged: false,
        tick: 0.,
        open_progress: 0.,
        level: 0.,
        account_index: 0,
        model_index: 0,
        shortcut_index: SHORTCUTS
            .iter()
            .position(|s| *s == app.settings.shortcut)
            .unwrap_or(0),
        microphones: microphones.clone(),
        microphone_index: microphones
            .iter()
            .position(|m| m.name == app.settings.microphone)
            .map(|i| i + 1)
            .unwrap_or(0),
        monitors: monitors.clone(),
        outputs: Vec::new(),
        monitor_index: monitors
            .iter()
            .position(|m| m.name == app.settings.monitor)
            .map(|i| i + 1)
            .unwrap_or(0),
        buffers: 0,
        dirty: true,
        max_x: monitor_width().unwrap_or(1280).saturating_sub(176),
        tab: 0,
        prompt_cursor: app.settings.prompt.chars().count(),
        prompt_focused: false,
        seat_keyboard: None,
        xkb_context: xkb::Context::new(xkb::CONTEXT_NO_FLAGS),
        xkb_state: None,
    };
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
        while let Ok(event) = rx.try_recv() {
            app.event(event, &mut ui);
            if ui.is_visible() {
                ui.dirty = true;
            }
        }
        app.tick(&mut ui);
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
        if (animating || color != ui.color || ui.dirty) && ui.configured {
            ui.tick += 1.;
            ui.color = color;
            ui.draw(&qh)?;
        }
        connection.flush()?;
        if let Some(guard) = queue.prepare_read() {
            let mut fd = libc::pollfd {
                fd: std::os::fd::AsRawFd::as_raw_fd(&connection.backend().poll_fd()),
                events: libc::POLLIN,
                revents: 0,
            };
            let ready = unsafe { libc::poll(&mut fd, 1, if animating { 16 } else { 100 }) };
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
        let mut ui = Ui {
            connection: connection.clone(),
            qh: qh.clone(),
            compositor: None,
            shm: None,
            seat: None,
            shell: None,
            manager: None,
            keyboard: None,
            surface: None,
            layer: None,
            modal_surface: None,
            modal_layer: None,
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
            configured: false,
            modal_configured: false,
            closed: false,
            color: [0, 0, 0],
            models: Vec::new(),
            message: String::new(),
            draft: crate::settings::Settings::load().unwrap_or_default(),
            panel: false,
            open_select: None,
            in_modal: false,
            pointer_xy: (0., 0.),
            press: None,
            dragged: false,
            tick: 0.,
            open_progress: 0.,
            level: 0.,
            account_index: 0,
            model_index: 0,
            shortcut_index: 0,
            microphones: Vec::new(),
            microphone_index: 0,
            monitors: Vec::new(),
            outputs: Vec::new(),
            monitor_index: 0,
            buffers: 0,
            dirty: false,
            max_x: 1280,
            tab: 0,
            prompt_cursor: 0,
            prompt_focused: false,
            seat_keyboard: None,
            xkb_context: xkb::Context::new(xkb::CONTEXT_NO_FLAGS),
            xkb_state: None,
        };
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
