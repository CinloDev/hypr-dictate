#[derive(Debug, PartialEq)]
pub enum Phase {
    Idle,
    Recording(String),
    Processing(String),
    Error,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum KeyAction {
    Ignore,
    StopRecording,
    CancelRecording,
    ReleaseAndInsert,
    ReleaseAndCancel,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum CaptureKey {
    Return,
    KpEnter,
    Escape,
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct KeyLifecycle {
    pub is_recording: bool,
    pub generation: u64,
    pub waiting_release: Option<CaptureKey>,
    pub deferred_result: Option<(u64, String, String)>,
    pub deferred_error: Option<(u64, String)>,
    pub target_address: Option<String>,
    pub dock_temporarily_shown: bool,
}

impl Default for KeyLifecycle {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyLifecycle {
    pub fn new() -> Self {
        Self {
            is_recording: false,
            generation: 0,
            waiting_release: None,
            deferred_result: None,
            deferred_error: None,
            target_address: None,
            dock_temporarily_shown: false,
        }
    }

    pub fn start(&mut self, address: String, dock_was_hidden: bool) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.is_recording = true;
        self.waiting_release = None;
        self.deferred_result = None;
        self.deferred_error = None;
        self.target_address = Some(address);
        self.dock_temporarily_shown = dock_was_hidden;
        self.generation
    }

    pub fn on_key_press(&mut self, key: CaptureKey) -> KeyAction {
        if !self.is_recording {
            return KeyAction::Ignore;
        }
        match key {
            CaptureKey::Return | CaptureKey::KpEnter => {
                self.is_recording = false;
                self.waiting_release = Some(key);
                KeyAction::StopRecording
            }
            CaptureKey::Escape => {
                self.is_recording = false;
                self.waiting_release = Some(key);
                self.deferred_result = None;
                self.deferred_error = None;
                self.generation = self.generation.wrapping_add(1);
                KeyAction::CancelRecording
            }
        }
    }

    pub fn on_key_release(&mut self, key: CaptureKey) -> KeyAction {
        if let Some(waiting) = self.waiting_release {
            let matched = match (waiting, key) {
                (CaptureKey::Return, CaptureKey::Return) => true,
                (CaptureKey::KpEnter, CaptureKey::KpEnter) => true,
                (CaptureKey::Escape, CaptureKey::Escape) => true,
                // Si el compositor normaliza enter o keypad enter:
                (CaptureKey::Return, CaptureKey::KpEnter) => true,
                (CaptureKey::KpEnter, CaptureKey::Return) => true,
                _ => false,
            };
            if matched {
                self.waiting_release = None;
                return match waiting {
                    CaptureKey::Return | CaptureKey::KpEnter => {
                        if self.deferred_error.is_some() {
                            KeyAction::ReleaseAndCancel
                        } else {
                            KeyAction::ReleaseAndInsert
                        }
                    }
                    CaptureKey::Escape => KeyAction::ReleaseAndCancel,
                };
            }
        }
        KeyAction::Ignore
    }

    pub fn on_api_finished(
        &mut self,
        generation: u64,
        address: String,
        text: String,
    ) -> Option<bool> {
        if generation != self.generation {
            return None;
        }
        if self.waiting_release.is_some() {
            self.deferred_result = Some((generation, address, text));
            Some(false)
        } else {
            Some(true)
        }
    }

    pub fn on_api_error(&mut self, generation: u64, error: String) -> Option<bool> {
        if generation != self.generation {
            return None;
        }
        if self.waiting_release.is_some() {
            self.deferred_error = Some((generation, error));
            Some(false)
        } else {
            Some(true)
        }
    }

    pub fn take_deferred(&mut self) -> Option<(String, String)> {
        if let Some((gen_id, addr, txt)) = self.deferred_result.take()
            && gen_id == self.generation
        {
            return Some((addr, txt));
        }
        None
    }

    pub fn take_deferred_error(&mut self) -> Option<String> {
        if let Some((gen_id, err)) = self.deferred_error.take()
            && gen_id == self.generation
        {
            return Some(err);
        }
        None
    }

    pub fn cancel(&mut self) {
        self.is_recording = false;
        self.generation = self.generation.wrapping_add(1);
        self.waiting_release = None;
        self.deferred_result = None;
        self.deferred_error = None;
        self.target_address = None;
        self.dock_temporarily_shown = false;
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct ArmingState {
    pub is_arming: bool,
    pub token: u64,
    pub seen_enter: bool,
    pub seen_modifiers: bool,
    pub mods_depressed: u32,
    pub pressed_keys: std::collections::BTreeSet<u32>,
    pub target: Option<String>,
}

impl Default for ArmingState {
    fn default() -> Self {
        Self::new()
    }
}

impl ArmingState {
    pub fn new() -> Self {
        Self {
            is_arming: false,
            token: 0,
            seen_enter: false,
            seen_modifiers: false,
            mods_depressed: 0,
            pressed_keys: std::collections::BTreeSet::new(),
            target: None,
        }
    }

    pub fn arm(&mut self, target: String) -> u64 {
        self.token = self.token.wrapping_add(1);
        self.is_arming = true;
        self.seen_enter = false;
        self.seen_modifiers = false;
        self.mods_depressed = 0;
        self.pressed_keys.clear();
        self.target = Some(target);
        self.token
    }

    pub fn on_enter(&mut self, is_dock_surface: bool, keys: &[u32]) {
        if !is_dock_surface || !self.is_arming {
            return;
        }
        self.seen_enter = true;
        self.pressed_keys.clear();
        for &k in keys {
            self.pressed_keys.insert(k);
        }
    }

    pub fn on_modifiers(&mut self, mods: u32) -> bool {
        if !self.is_arming {
            return false;
        }
        if self.seen_enter {
            self.seen_modifiers = true;
        }
        self.mods_depressed = mods;
        self.check_ready()
    }

    pub fn on_key_pressed(&mut self, key: u32) {
        if self.is_arming {
            self.pressed_keys.insert(key);
        }
    }

    pub fn on_key_released(&mut self, key: u32) -> bool {
        if self.is_arming {
            self.pressed_keys.remove(&key);
            self.check_ready()
        } else {
            false
        }
    }

    pub fn check_ready(&mut self) -> bool {
        if self.is_arming
            && self.seen_enter
            && self.seen_modifiers
            && self.mods_depressed == 0
            && self.pressed_keys.is_empty()
        {
            self.is_arming = false;
            true
        } else {
            false
        }
    }

    pub fn cancel(&mut self) {
        self.token = self.token.wrapping_add(1);
        self.is_arming = false;
        self.seen_enter = false;
        self.seen_modifiers = false;
        self.target = None;
    }
}

impl Phase {
    pub fn start(&mut self, address: &str) -> bool {
        if !matches!(self, Self::Idle | Self::Error) || !valid_address(address) {
            return false;
        }
        *self = Self::Recording(address.into());
        true
    }
    pub fn stop(&mut self) -> Option<String> {
        if let Self::Recording(address) = self {
            let address = address.clone();
            *self = Self::Processing(address.clone());
            Some(address)
        } else {
            None
        }
    }
}

pub fn valid_address(address: &str) -> bool {
    address.strip_prefix("0x").is_some_and(|s| {
        !s.is_empty()
            && s.len() <= 16
            && s.bytes().all(|b| b.is_ascii_hexdigit())
            && s.bytes().any(|b| b != b'0')
    })
}

pub fn wav(pcm: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(44 + pcm.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&16000u32.to_le_bytes());
    out.extend_from_slice(&32000u32.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
    out.extend_from_slice(pcm);
    out
}

pub fn transient_wayland_error(error: &wayland_client::backend::WaylandError) -> bool {
    matches!(error, wayland_client::backend::WaylandError::Io(error) if error.kind() == std::io::ErrorKind::WouldBlock)
}

pub fn capture_valid(code: Option<i32>, signal: Option<i32>, stderr: &[u8], pcm: &[u8]) -> bool {
    pcm.len() >= 3200
        && pcm.len() <= 9_600_000
        && pcm.len().is_multiple_of(2)
        && (code == Some(0)
            || code == Some(130)
            || signal == Some(libc::SIGINT)
            || (code == Some(1) && stderr.is_empty()))
}

pub fn focus_dispatch(address: &str) -> String {
    format!("hl.dsp.focus({{window = \"address:{address}\"}})")
}

pub const SAFE_EVDEV_KEYS: [u32; 10] = [2, 3, 4, 5, 6, 7, 8, 9, 10, 11];

#[derive(Debug, PartialEq, Clone)]
pub struct ChunkPlan {
    pub chunk_text: String,
    pub keymap: String,
    pub keycodes: Vec<u32>,
}

pub fn plan_chunks(text: &str) -> Vec<ChunkPlan> {
    let cleaned = sanitize_dictated_insertion(text);
    if cleaned.is_empty() {
        return Vec::new();
    }

    let all_chars: Vec<char> = cleaned.chars().collect();
    let mut cursor = 0;
    let mut out = Vec::new();

    while cursor < all_chars.len() {
        let mut map = std::collections::BTreeMap::new();
        let mut names = String::new();
        let mut symbols = String::new();
        let mut end = cursor;

        while end < all_chars.len() {
            let ch = all_chars[end];
            if !map.contains_key(&ch) {
                if map.len() == SAFE_EVDEV_KEYS.len() {
                    break;
                }
                let slot = map.len();
                let evdev = SAFE_EVDEV_KEYS[slot];
                let xkb = evdev + 8;
                map.insert(ch, evdev);
                names.push_str(&format!("<K{slot}> = {xkb};\n"));
                symbols.push_str(&format!("key <K{slot}> {{ [ U{:04X} ] }};\n", ch as u32));
            }
            end += 1;
        }

        let keymap = format!(
            "xkb_keymap {{ xkb_keycodes \"dictate\" {{ minimum = 8; maximum = 255; {names} }}; xkb_types \"dictate\" {{ type \"ONE_LEVEL\" {{ modifiers = None; level_name[Level1] = \"Any\"; }}; }}; xkb_compatibility \"dictate\" {{}}; xkb_symbols \"dictate\" {{ {symbols} }}; }};\n\0"
        );

        let keycodes = all_chars[cursor..end].iter().map(|ch| map[ch]).collect();
        let chunk_text = all_chars[cursor..end].iter().collect();

        out.push(ChunkPlan {
            chunk_text,
            keymap,
            keycodes,
        });

        cursor = end;
    }

    out
}

fn normalize_char(c: char) -> char {
    match c {
        'á' | 'Á' => 'a',
        'é' | 'É' => 'e',
        'í' | 'Í' => 'i',
        'ó' | 'Ó' => 'o',
        'ú' | 'Ú' | 'ü' | 'Ü' => 'u',
        other => other.to_ascii_lowercase(),
    }
}

pub fn clean_repeated_words(text: &str) -> String {
    let mut tokens: Vec<(String, bool)> = Vec::new();
    let mut current = String::new();
    let mut is_word: Option<bool> = None;

    for ch in text.chars() {
        let ch_is_word = ch.is_alphanumeric();
        match is_word {
            None => {
                is_word = Some(ch_is_word);
                current.push(ch);
            }
            Some(w) if w == ch_is_word => {
                current.push(ch);
            }
            Some(_) => {
                tokens.push((std::mem::take(&mut current), is_word.unwrap()));
                current.push(ch);
                is_word = Some(ch_is_word);
            }
        }
    }
    if !current.is_empty() {
        tokens.push((current, is_word.unwrap_or(false)));
    }

    let mut out_tokens: Vec<(String, bool)> = Vec::new();
    let mut last_word_norm: Option<String> = None;

    for (token_text, token_is_word) in tokens {
        if token_is_word {
            let norm: String = token_text.chars().map(normalize_char).collect();
            if let Some(prev) = &last_word_norm
                && prev == &norm
            {
                // Accidental immediate repetition! Drop the separator preceding it
                if let Some((_, false)) = out_tokens.last() {
                    out_tokens.pop();
                }
                continue;
            }
            last_word_norm = Some(norm);
            out_tokens.push((token_text, true));
        } else {
            out_tokens.push((token_text, false));
        }
    }

    // Assemble and normalize redundant punctuation/spacing
    let mut result = String::with_capacity(text.len());
    let mut prev_char: Option<char> = None;

    for (t, _) in out_tokens {
        for ch in t.chars() {
            // Avoid duplicate punctuation like "..", ",,", "??"
            if matches!(ch, '.' | ',' | ';' | ':' | '!' | '?')
                && prev_char == Some(ch)
            {
                continue;
            }
            // Avoid space immediately before punctuation: "word ." -> "word."
            if matches!(ch, '.' | ',' | ';' | ':' | '!' | '?')
                && prev_char == Some(' ')
            {
                result.pop();
            }
            result.push(ch);
            prev_char = Some(ch);
        }
    }

    result
}

pub fn is_line_break(ch: char) -> bool {
    matches!(
        ch,
        '\n' | '\r' | '\u{000B}' | '\u{000C}' | '\u{0085}' | '\u{2028}' | '\u{2029}'
    )
}

pub fn sanitize_dictated_insertion(text: &str) -> String {
    let converted: String = text
        .chars()
        .map(|c| if is_line_break(c) { ' ' } else { c })
        .collect();
    sanitize_plain_text(&converted)
}

pub fn sanitize_plain_text(text: &str) -> String {
    let deduplicated = clean_repeated_words(text);
    let mut out = String::with_capacity(deduplicated.len());
    let mut prev_space = false;
    for ch in deduplicated.chars() {
        let mapped = if matches!(ch, '\n' | '\r' | '\t') {
            ' '
        } else if ch.is_control() {
            continue;
        } else {
            ch
        };
        if mapped == ' ' {
            if !prev_space && !out.is_empty() {
                out.push(' ');
            }
            prev_space = true;
        } else {
            out.push(mapped);
            prev_space = false;
        }
    }
    out.trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_lifecycle_defers_fast_api_result_until_enter_released() {
        let mut lc = KeyLifecycle::new();
        let gen_id = lc.start("0x1234abcd".into(), true);
        assert!(lc.is_recording);
        assert!(lc.dock_temporarily_shown);

        // User presses Enter to finish
        let action = lc.on_key_press(CaptureKey::Return);
        assert_eq!(action, KeyAction::StopRecording);
        assert!(!lc.is_recording);
        assert_eq!(lc.waiting_release, Some(CaptureKey::Return));

        // API finishes fast, BEFORE Enter key is physically released
        let ready = lc.on_api_finished(gen_id, "0x1234abcd".into(), "Texto rápido".into());
        assert_eq!(ready, Some(false), "Debe diferir la inserción mientras la tecla Enter esté presionada");
        assert_eq!(
            lc.deferred_result,
            Some((gen_id, "0x1234abcd".into(), "Texto rápido".into()))
        );

        // User physically releases Enter
        let rel_action = lc.on_key_release(CaptureKey::Return);
        assert_eq!(rel_action, KeyAction::ReleaseAndInsert);
        assert_eq!(lc.waiting_release, None);

        // Deferred result is now available for typing
        let deferred = lc.take_deferred();
        assert_eq!(deferred, Some(("0x1234abcd".into(), "Texto rápido".into())));
    }

    #[test]
    fn key_lifecycle_handles_enter_release_before_api_finishes() {
        let mut lc = KeyLifecycle::new();
        let gen_id = lc.start("0x1234abcd".into(), false);

        // User presses Enter
        let action = lc.on_key_press(CaptureKey::Return);
        assert_eq!(action, KeyAction::StopRecording);

        // User releases Enter before API responds
        let rel_action = lc.on_key_release(CaptureKey::Return);
        assert_eq!(rel_action, KeyAction::ReleaseAndInsert);
        assert_eq!(lc.waiting_release, None);
        assert_eq!(lc.take_deferred(), None);

        // Later, API finishes
        let ready = lc.on_api_finished(gen_id, "0x1234abcd".into(), "Texto normal".into());
        assert_eq!(ready, Some(true), "Debe insertar inmediatamente si la tecla ya fue liberada");
    }

    #[test]
    fn arming_waits_until_modifiers_and_keys_released() {
        let mut arm = ArmingState::new();
        arm.arm("0x1234abcd".into());
        assert!(!arm.check_ready(), "Estado limpio inicial sin Enter/Modifiers NO debe armar");

        // Compositor entrega Enter con tecla V (47) retenida en superficie válida
        arm.on_enter(true, &[47]);
        assert!(!arm.check_ready(), "Enter sin Modifiers no debe armar");

        // Compositor entrega Modifiers con SUPER+ALT (72)
        assert!(!arm.on_modifiers(72));

        // Liberar tecla V
        assert!(!arm.on_key_released(47));

        // Liberar modificadores
        assert!(arm.on_modifiers(0), "Debe transicionar a listo al soltar modificadores tras Enter");
        assert!(!arm.is_arming);
    }

    #[test]
    fn arming_ignores_stale_or_modal_surface_enter() {
        let mut arm = ArmingState::new();
        arm.arm("0x1234abcd".into());

        // Enter de superficie obsoleta o modal
        arm.on_enter(false, &[]);
        assert!(!arm.seen_enter, "No debe registrar seen_enter para superficie no coincidente");
        assert!(!arm.check_ready());
    }

    #[test]
    fn arming_cancel_increments_token_and_invalidates_readiness() {
        let mut arm = ArmingState::new();
        let tok1 = arm.arm("0x1234abcd".into());
        arm.cancel();
        assert_ne!(tok1, arm.token);
        assert!(!arm.check_ready());
        assert!(!arm.is_arming);
        assert_eq!(arm.target, None);
    }

    #[test]
    fn key_lifecycle_failure_before_release_retains_key_until_release() {
        let mut lc = KeyLifecycle::new();
        let gen_id = lc.start("0x1234abcd".into(), true);

        // Enter pressed
        let action = lc.on_key_press(CaptureKey::Return);
        assert_eq!(action, KeyAction::StopRecording);
        assert_eq!(lc.waiting_release, Some(CaptureKey::Return));

        // Fast API failure occurs while key is still physically held
        let immediate = lc.on_api_error(gen_id, "HTTP 500".into());
        assert_eq!(immediate, Some(false), "El error debe diferirse mientras la tecla siga presionada");
        assert_eq!(lc.waiting_release, Some(CaptureKey::Return), "No debe destruir waiting_release");
        assert_eq!(lc.deferred_error, Some((gen_id, "HTTP 500".into())));

        // User releases Enter
        let rel_action = lc.on_key_release(CaptureKey::Return);
        assert_eq!(rel_action, KeyAction::ReleaseAndCancel);
        assert_eq!(lc.waiting_release, None);
        assert_eq!(lc.take_deferred_error(), Some("HTTP 500".into()));
    }

    #[test]
    fn key_lifecycle_release_before_failure_triggers_immediate_error() {
        let mut lc = KeyLifecycle::new();
        let gen_id = lc.start("0x1234abcd".into(), false);

        // Enter pressed then released
        lc.on_key_press(CaptureKey::Return);
        let rel_action = lc.on_key_release(CaptureKey::Return);
        assert_eq!(rel_action, KeyAction::ReleaseAndInsert);
        assert_eq!(lc.waiting_release, None);

        // Later API error occurs
        let immediate = lc.on_api_error(gen_id, "HTTP 502".into());
        assert_eq!(immediate, Some(true), "Si la tecla ya fue liberada, debe procesar error de inmediato");
    }

    #[test]
    fn key_lifecycle_rejects_stale_result_after_cancel() {
        let mut lc = KeyLifecycle::new();
        let old_gen = lc.start("0x1234abcd".into(), true);

        // Cancel resets lifecycle and increments generation
        lc.cancel();
        assert_ne!(lc.generation, old_gen);

        // Late API response arrives from abandoned request
        let res = lc.on_api_finished(old_gen, "0x1234abcd".into(), "Texto viejo".into());
        assert_eq!(res, None, "Debe rechazar resultados con generación obsoleta tras cancel");
        assert_eq!(lc.take_deferred(), None);
    }

    #[test]
    fn key_lifecycle_rejects_stale_result_from_prior_recording_at_same_address() {
        let mut lc = KeyLifecycle::new();
        let gen1 = lc.start("0x1234abcd".into(), true);
        lc.on_key_press(CaptureKey::Escape);
        lc.on_key_release(CaptureKey::Escape);

        // Start new recording at the same window address
        let gen2 = lc.start("0x1234abcd".into(), true);
        assert_ne!(gen1, gen2);

        // Stale result from gen1 arrives
        let stale = lc.on_api_finished(gen1, "0x1234abcd".into(), "Texto de sesión previa".into());
        assert_eq!(stale, None, "Resultado de sesión previa debe ser descartado");

        // Fresh result from gen2 arrives
        let fresh = lc.on_api_finished(gen2, "0x1234abcd".into(), "Texto actual".into());
        assert_eq!(fresh, Some(true));
    }

    #[test]
    fn dictated_insertion_converts_all_line_breaks_to_spaces_strictly_single_line() {
        let raw = "Línea 1\r\nLínea 2\nLínea 3\rLínea 4\u{0085}Línea 5\u{2028}Línea 6\u{2029}Fin";
        let cleaned = super::sanitize_dictated_insertion(raw);
        assert_eq!(cleaned, "Línea 1 Línea 2 Línea 3 Línea 4 Línea 5 Línea 6 Fin");
        for ch in cleaned.chars() {
            assert!(!super::is_line_break(ch), "No debe contener ningún tipo de salto de línea");
        }
    }

    #[test]
    fn plan_chunks_strictly_enforces_single_line_for_crlf_and_unicode_separators() {
        let raw = "Intro\r\nSegunda\u{2028}Tercera\u{2029}Cuarta";
        let plans = super::plan_chunks(raw);
        assert!(!plans.is_empty());
        for plan in plans {
            for ch in plan.chunk_text.chars() {
                assert!(!super::is_line_break(ch), "El chunk text no debe contener ningún salto de línea");
            }
        }
    }

    #[test]
    fn key_lifecycle_kp_enter_behaves_identically() {
        let mut lc = KeyLifecycle::new();
        lc.start("0x1234abcd".into(), false);

        let action = lc.on_key_press(CaptureKey::KpEnter);
        assert_eq!(action, KeyAction::StopRecording);
        assert_eq!(lc.waiting_release, Some(CaptureKey::KpEnter));

        let rel_action = lc.on_key_release(CaptureKey::KpEnter);
        assert_eq!(rel_action, KeyAction::ReleaseAndInsert);
        assert_eq!(lc.waiting_release, None);
    }

    #[test]
    fn key_lifecycle_escape_cancels_without_api() {
        let mut lc = KeyLifecycle::new();
        lc.start("0x1234abcd".into(), true);

        // User presses Escape
        let action = lc.on_key_press(CaptureKey::Escape);
        assert_eq!(action, KeyAction::CancelRecording);
        assert!(!lc.is_recording);
        assert_eq!(lc.waiting_release, Some(CaptureKey::Escape));

        // Release Escape
        let rel_action = lc.on_key_release(CaptureKey::Escape);
        assert_eq!(rel_action, KeyAction::ReleaseAndCancel);
        assert_eq!(lc.waiting_release, None);
        assert_eq!(lc.take_deferred(), None);
    }

    #[test]
    fn key_lifecycle_ignores_unrelated_releases_and_cancel_resets_all() {
        let mut lc = KeyLifecycle::new();
        lc.start("0x1234abcd".into(), true);

        assert_eq!(lc.on_key_release(CaptureKey::Return), KeyAction::Ignore);

        lc.on_key_press(CaptureKey::Return);
        // Release Escape while waiting for Return -> ignored
        assert_eq!(lc.on_key_release(CaptureKey::Escape), KeyAction::Ignore);
        assert_eq!(lc.waiting_release, Some(CaptureKey::Return));

        // Cancel
        lc.cancel();
        assert!(!lc.is_recording);
        assert_eq!(lc.waiting_release, None);
        assert_eq!(lc.deferred_result, None);
        assert_eq!(lc.target_address, None);
        assert!(!lc.dock_temporarily_shown);
    }
    #[test]
    fn chunks_preserve_entire_text_with_arbitrary_unique_characters() {
        let sentence = "Genial, funciona muy bien. Está perfecto, así que bueno, esto es una transcripción de prueba.";
        let chunks = super::plan_chunks(sentence);
        assert!(!chunks.is_empty());

        let reconstructed: String = chunks.iter().map(|c| c.chunk_text.as_str()).collect();
        assert_eq!(reconstructed, super::sanitize_plain_text(sentence));

        let context = xkbcommon::xkb::Context::new(xkbcommon::xkb::CONTEXT_NO_FLAGS);
        for chunk in &chunks {
            assert!(chunk.keycodes.len() == chunk.chunk_text.chars().count());
            let km = xkbcommon::xkb::Keymap::new_from_string(
                &context,
                chunk.keymap.trim_end_matches('\0').to_string(),
                xkbcommon::xkb::KEYMAP_FORMAT_TEXT_V1,
                xkbcommon::xkb::KEYMAP_COMPILE_NO_FLAGS,
            );
            assert!(km.is_some(), "Cada chunk debe generar un keymap XKB sintácticamente válido");
        }
    }

    #[test]
    fn chunks_handle_unlimited_unique_characters_without_error_or_loss() {
        let mut alphabet = String::new();
        for i in 0..120 {
            alphabet.push(char::from_u32(0x3000 + i as u32).unwrap());
        }
        let chunks = super::plan_chunks(&alphabet);
        assert_eq!(chunks.len(), 12); // 120 caracteres únicos / 10 por chunk = 12 chunks
        let reconstructed: String = chunks.iter().map(|c| c.chunk_text.as_str()).collect();
        assert_eq!(reconstructed, alphabet);
    }

    #[test]
    fn cleans_accidental_speech_repetitions_and_duplicate_words() {
        let input = "Genial. Genial, funciona muy bien. Está Está perfecto, así que bueno, esto es una transcripción de prueba para que veas cómo está transcribiendo.";
        let cleaned = super::clean_repeated_words(input);
        assert_eq!(
            cleaned,
            "Genial, funciona muy bien. Está perfecto, así que bueno, esto es una transcripción de prueba para que veas cómo está transcribiendo."
        );
        assert_eq!(
            super::clean_repeated_words("Hola, hola mundo."),
            "Hola mundo."
        );
        assert_eq!(
            super::clean_repeated_words("vamos vamos a ver"),
            "vamos a ver"
        );
        assert_eq!(
            super::clean_repeated_words("Esto es una prueba normal."),
            "Esto es una prueba normal."
        );
    }
    #[test]
    fn plain_text_sanitization_removes_newlines_tabs_and_dangerous_keycodes() {
        assert_eq!(
            super::sanitize_plain_text("Hola\nmundo\tbonito\r\n"),
            "Hola mundo bonito"
        );
        // Must never include ESC(1), TAB(15), Q(16), W(17), ENTER(28), F4(62)
        for forbidden in [1, 14, 15, 16, 17, 28, 62] {
            assert!(!super::SAFE_EVDEV_KEYS.contains(&forbidden));
        }
    }
    #[test]
    fn focus_uses_current_hyprland_dispatch_syntax() {
        assert_eq!(
            super::focus_dispatch("0xabc"),
            "hl.dsp.focus({window = \"address:0xabc\"})"
        );
    }
    #[test]
    fn pipewire_exit_one_requires_valid_audio_and_no_error() {
        assert!(super::capture_valid(Some(1), None, b"", &[0; 3200]));
        assert!(!super::capture_valid(
            Some(1),
            None,
            b"connection failed",
            &[0; 3200]
        ));
        assert!(!super::capture_valid(Some(1), None, b"", &[]));
        assert!(!super::capture_valid(Some(2), None, b"", &[0; 3200]));
    }
    #[test]
    fn nonblocking_wayland_read_is_not_fatal() {
        use wayland_client::backend::WaylandError;
        assert!(super::transient_wayland_error(&WaylandError::Io(
            std::io::ErrorKind::WouldBlock.into()
        )));
        assert!(!super::transient_wayland_error(&WaylandError::Io(
            std::io::ErrorKind::BrokenPipe.into()
        )));
    }
    #[test]
    fn target_is_fixed_until_completion() {
        let mut phase = Phase::Idle;
        assert!(phase.start("0xabc"));
        assert!(!phase.start("0xdef"));
        assert_eq!(phase.stop(), Some("0xabc".into()));
        assert_eq!(phase, Phase::Processing("0xabc".into()));
        assert_eq!(phase.stop(), None);
    }
    #[test]
    fn invalid_target_never_starts_recording() {
        for address in ["", "0x0", "0xabc;evil", "not-an-address"] {
            let mut phase = Phase::Idle;
            assert!(!phase.start(address));
        }
    }
    #[test]
    fn wav_has_correct_lengths_and_format() {
        let audio = wav(&[0; 320]);
        assert_eq!(&audio[..4], b"RIFF");
        assert_eq!(audio.len(), 364);
        assert_eq!(u32::from_le_bytes(audio[4..8].try_into().unwrap()), 356);
        assert_eq!(u32::from_le_bytes(audio[24..28].try_into().unwrap()), 16000);
        assert_eq!(u32::from_le_bytes(audio[40..44].try_into().unwrap()), 320);
    }
}
